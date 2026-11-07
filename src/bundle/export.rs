//! Bundle export: from a saved local session, or read-only from a saved
//! SSH machine through the live API snapshot.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::git_info;
use super::manifest::{FolderInfo, Manifest, ManifestAgent, ManifestPane, SourceInfo};
use super::stores::{self, StoreKind, StoreRoots};
use super::{
    copy_file, home_dir, message_error, restrict_dir_permissions, BundleResult, API_SNAPSHOT_FILE,
    HISTORY_FILE, MAX_SCROLLBACK_BYTES, SESSION_FILE,
};

#[derive(Debug, Clone)]
pub(crate) struct ExportOptions {
    pub(crate) output: PathBuf,
    pub(crate) force: bool,
    pub(crate) include_agent_files: bool,
    pub(crate) include_scrollback: bool,
}

/// One pane's visible text, captured by the CLI layer through the socket API
/// (`pane.read`) so this module never talks to a server directly.
#[derive(Debug, Clone)]
pub(crate) struct ScrollbackEntry {
    pub(crate) pane_id: String,
    pub(crate) text: String,
}

#[derive(Debug)]
pub(crate) struct ExportOutcome {
    pub(crate) panes: usize,
    pub(crate) agents_copied: usize,
    pub(crate) agents_missing_files: usize,
    pub(crate) warnings: Vec<String>,
}

fn source_info(session_name: Option<String>, machine_label: Option<String>) -> SourceInfo {
    SourceInfo {
        herdr_version: env!("CARGO_PKG_VERSION").to_string(),
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        home: home_dir().map(|home| home.display().to_string()),
        session_name,
        machine_label,
    }
}

fn prepare_output(output: &Path, force: bool) -> BundleResult<()> {
    if output.is_dir() {
        let mut entries = std::fs::read_dir(output)?;
        if entries.next().is_some() && !force {
            return Err(message_error(format!(
                "{} exists and is not empty (use --force to write bundle files into it)",
                output.display()
            )));
        }
    } else if output.exists() {
        return Err(message_error(format!(
            "{} exists and is not a directory",
            output.display()
        )));
    }
    std::fs::create_dir_all(output)?;
    restrict_dir_permissions(output)?;
    Ok(())
}

fn pane_dir_name(pane_id: &str) -> String {
    pane_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

fn write_scrollbacks(
    bundle_dir: &Path,
    scrollbacks: &[ScrollbackEntry],
    manifest: &mut Manifest,
) -> BundleResult<()> {
    for entry in scrollbacks {
        if entry.text.is_empty() {
            continue;
        }
        let relative = format!("panes/{}/scrollback.txt", pane_dir_name(&entry.pane_id));
        let target = stores::bundle_path(bundle_dir, &relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = tail_on_char_boundary(&entry.text, MAX_SCROLLBACK_BYTES);
        std::fs::write(&target, text)?;
        if let Some(pane) = manifest
            .panes
            .iter_mut()
            .find(|pane| pane.pane == entry.pane_id)
        {
            pane.scrollback = Some(relative);
        }
    }
    Ok(())
}

fn tail_on_char_boundary(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut start = text.len() - max_bytes;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

struct AgentCopy {
    manifest: ManifestAgent,
    copied: bool,
}

/// Resolve one pane's agent for the manifest and copy its session files.
/// Failures are recorded in the manifest, never fatal: a bundle with layout
/// and references is still useful when one transcript cannot be found.
fn copy_agent_session(
    agent: ManifestAgent,
    store: Option<(StoreKind, String)>,
    cwd: &str,
    roots: Option<&StoreRoots>,
    bundle_dir: &Path,
    include_files: bool,
    remote: bool,
) -> AgentCopy {
    let mut agent = agent;
    if remote {
        agent.files_missing = Some("remote_not_transferred".to_string());
        return AgentCopy {
            manifest: agent,
            copied: false,
        };
    }
    if !include_files {
        agent.files_missing = Some("disabled".to_string());
        return AgentCopy {
            manifest: agent,
            copied: false,
        };
    }
    let Some((kind, value)) = store else {
        agent.files_missing = Some("unsupported_store".to_string());
        return AgentCopy {
            manifest: agent,
            copied: false,
        };
    };
    let Some(roots) = roots else {
        agent.files_missing = Some("not_found".to_string());
        return AgentCopy {
            manifest: agent,
            copied: false,
        };
    };
    let Some(located) = stores::locate(kind, &value, cwd, roots) else {
        agent.files_missing = Some("not_found".to_string());
        return AgentCopy {
            manifest: agent,
            copied: false,
        };
    };
    match stores::copy_into_bundle(kind, &located, &agent.agent, &value, bundle_dir) {
        Ok(relative) => {
            agent.bundle_path = Some(relative);
            AgentCopy {
                manifest: agent,
                copied: true,
            }
        }
        Err(_) => {
            agent.files_missing = Some("not_found".to_string());
            AgentCopy {
                manifest: agent,
                copied: false,
            }
        }
    }
}

fn finish_export(
    options: &ExportOptions,
    mut manifest: Manifest,
    scrollbacks: &[ScrollbackEntry],
    agents_copied: usize,
) -> BundleResult<ExportOutcome> {
    if options.include_scrollback {
        write_scrollbacks(&options.output, scrollbacks, &mut manifest)?;
    }
    let agents_missing_files = manifest
        .panes
        .iter()
        .filter(|pane| {
            pane.agent
                .as_ref()
                .is_some_and(|agent| agent.files_missing.is_some())
        })
        .count();
    for pane in &manifest.panes {
        if let Some(agent) = &pane.agent {
            if let Some(reason) = &agent.files_missing {
                manifest.warnings.push(format!(
                    "{}: {} session files not included ({reason}); the pane restores as a shell unless that session already exists on the target",
                    pane.pane, agent.agent
                ));
            }
        }
    }
    let outcome = ExportOutcome {
        panes: manifest.panes.len(),
        agents_copied,
        agents_missing_files,
        warnings: manifest.warnings.clone(),
    };
    manifest.write_to(&options.output)?;
    std::fs::write(
        options.output.join("README.md"),
        super::manifest::render_readme(&manifest),
    )?;
    Ok(outcome)
}

fn collect_folders(cwds: &[String]) -> BTreeMap<String, FolderInfo> {
    let mut folders = BTreeMap::new();
    for cwd in cwds {
        folders.entry(cwd.clone()).or_insert_with(|| FolderInfo {
            git: git_info::provenance(Path::new(cwd)),
        });
    }
    folders
}

fn session_kind_name(kind: crate::agent_resume::AgentSessionRefKind) -> &'static str {
    match kind {
        crate::agent_resume::AgentSessionRefKind::Id => "id",
        crate::agent_resume::AgentSessionRefKind::Path => "path",
    }
}

/// Export the saved session in `session_dir` (a Herdr session data
/// directory). Works whether or not the server is currently running;
/// `scrollbacks` is empty when no server could be read.
pub(crate) fn export_persisted(
    session_dir: &Path,
    session_name: &str,
    roots: Option<&StoreRoots>,
    scrollbacks: &[ScrollbackEntry],
    options: &ExportOptions,
) -> BundleResult<ExportOutcome> {
    let session_path = session_dir.join(SESSION_FILE);
    let content = std::fs::read_to_string(&session_path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            message_error(format!(
                "no saved session at {}; start Herdr once so it can save its layout",
                session_path.display()
            ))
        } else {
            super::BundleError::Io(error)
        }
    })?;
    let snapshot = crate::persist::parse_snapshot(&content)
        .map_err(|error| message_error(format!("saved session is unreadable: {error}")))?;

    prepare_output(&options.output, options.force)?;
    std::fs::write(options.output.join(SESSION_FILE), &content)?;
    let history_path = session_dir.join(HISTORY_FILE);
    if history_path.is_file() {
        copy_file(&history_path, &options.output.join(HISTORY_FILE))?;
    }

    let mut manifest = Manifest::new(
        source_info(Some(session_name.to_string()), None),
        "persisted",
    );
    let mut agents_copied = 0usize;
    let mut cwds = Vec::new();
    for (ws_index, workspace) in snapshot.workspaces.iter().enumerate() {
        let workspace_label = workspace
            .id
            .clone()
            .unwrap_or_else(|| format!("w{}", ws_index + 1));
        for (tab_index, tab) in workspace.tabs.iter().enumerate() {
            let tab_number = workspace
                .public_tab_numbers
                .get(tab_index)
                .copied()
                .unwrap_or(tab_index + 1);
            let tab_label = format!(
                "{workspace_label}:t{}",
                crate::workspace::encode_public_number(tab_number)
            );
            let mut pane_ids: Vec<u32> = tab.panes.keys().copied().collect();
            pane_ids.sort_unstable();
            for pane_id in pane_ids {
                let Some(pane) = tab.panes.get(&pane_id) else {
                    continue;
                };
                let pane_label = workspace
                    .public_pane_numbers
                    .get(&pane_id)
                    .map(|number| {
                        crate::workspace::public_pane_id_for_number(&workspace_label, *number)
                    })
                    .unwrap_or_else(|| format!("{tab_label}/pane-{pane_id}"));
                let cwd = pane.cwd.display().to_string();
                cwds.push(cwd.clone());
                let (kind, agent) = persisted_agent(
                    pane,
                    &cwd,
                    roots,
                    &options.output,
                    options.include_agent_files,
                    &mut agents_copied,
                );
                manifest.panes.push(ManifestPane {
                    workspace: workspace_label.clone(),
                    tab: tab_label.clone(),
                    pane: pane_label,
                    kind: kind.to_string(),
                    cwd,
                    agent,
                    scrollback: None,
                });
            }
        }
    }
    manifest.folders = collect_folders(&cwds);
    finish_export(options, manifest, scrollbacks, agents_copied)
}

fn persisted_agent(
    pane: &crate::persist::PaneSnapshot,
    cwd: &str,
    roots: Option<&StoreRoots>,
    bundle_dir: &Path,
    include_files: bool,
    agents_copied: &mut usize,
) -> (&'static str, Option<ManifestAgent>) {
    if let Some(session) = &pane.agent_session {
        let session_ref = crate::agent_resume::session_ref_from_snapshot(
            &session.source,
            &session.agent,
            session.kind,
            &session.value,
        )
        .map(|persisted| persisted.session_ref);
        let resume_argv = session_ref
            .as_ref()
            .and_then(|reference| {
                crate::agent_resume::plan(&session.source, &session.agent, reference)
            })
            .map(|plan| plan.argv);
        let store = stores::kind_for(&session.agent, &session.kind)
            .map(|kind| (kind, session.value.clone()));
        let copy = copy_agent_session(
            ManifestAgent {
                source: Some(session.source.clone()),
                agent: session.agent.clone(),
                session_kind: Some(session_kind_name(session.kind).to_string()),
                session_value: Some(session.value.clone()),
                resume_argv,
                bundle_path: None,
                files_missing: None,
            },
            store,
            cwd,
            roots,
            bundle_dir,
            include_files,
            false,
        );
        *agents_copied += usize::from(copy.copied);
        return ("agent", Some(copy.manifest));
    }
    if let Some(resume) = &pane.agent_resume {
        let muse_id = stores::muse_session_id(&resume.argv);
        let store = muse_id.as_ref().map(|id| (StoreKind::Muse, id.clone()));
        let copy = copy_agent_session(
            ManifestAgent {
                source: Some(resume.source.clone()),
                agent: resume.agent.clone(),
                session_kind: muse_id.as_ref().map(|_| "id".to_string()),
                session_value: muse_id,
                resume_argv: Some(resume.argv.clone()),
                bundle_path: None,
                files_missing: None,
            },
            store,
            cwd,
            roots,
            bundle_dir,
            include_files,
            false,
        );
        *agents_copied += usize::from(copy.copied);
        return ("agent", Some(copy.manifest));
    }
    if pane.launch_argv.is_some() {
        return ("program", None);
    }
    ("shell", None)
}

/// Export a saved SSH machine's live session without modifying it. Only the
/// socket API is read: layout, agent references, and pane text. Agent
/// transcript files stay on that machine (the API has no file transfer), so
/// the manifest says so and import rebuilds the layout from the pane list.
/// For a full bundle including transcripts, run `herdr session export` on
/// the machine itself.
pub(crate) fn export_remote(
    snapshot: &crate::api::schema::SessionSnapshot,
    machine_label: &str,
    scrollbacks: &[ScrollbackEntry],
    options: &ExportOptions,
) -> BundleResult<ExportOutcome> {
    prepare_output(&options.output, options.force)?;
    let snapshot_json = serde_json::to_string_pretty(snapshot)
        .map_err(|error| super::BundleError::Io(std::io::Error::other(error)))?;
    std::fs::write(options.output.join(API_SNAPSHOT_FILE), snapshot_json + "\n")?;

    let mut manifest = Manifest::new(
        source_info(None, Some(machine_label.to_string())),
        "remote_api",
    );
    manifest.warnings.push(format!(
        "captured read-only from machine '{machine_label}'; agent transcript files were not transferred. Run `herdr session export` on that machine for a bundle that includes them"
    ));
    for pane in &snapshot.panes {
        let cwd = pane
            .foreground_cwd
            .clone()
            .or_else(|| pane.cwd.clone())
            .unwrap_or_default();
        let agent = pane.agent_session.as_ref().map(|session| {
            let resume_argv = crate::agent_resume::session_ref_from_snapshot(
                &session.source,
                &session.agent,
                session.kind,
                &session.value,
            )
            .and_then(|persisted| {
                crate::agent_resume::plan(&session.source, &session.agent, &persisted.session_ref)
            })
            .map(|plan| plan.argv);
            ManifestAgent {
                source: Some(session.source.clone()),
                agent: session.agent.clone(),
                session_kind: Some(session_kind_name(session.kind).to_string()),
                session_value: Some(session.value.clone()),
                resume_argv,
                bundle_path: None,
                files_missing: Some("remote_not_transferred".to_string()),
            }
        });
        let kind = if agent.is_some() || pane.agent.is_some() {
            "agent"
        } else {
            "shell"
        };
        manifest
            .folders
            .entry(cwd.clone())
            .or_insert(FolderInfo { git: None });
        manifest.panes.push(ManifestPane {
            workspace: pane.workspace_id.clone(),
            tab: pane.tab_id.clone(),
            pane: pane.pane_id.clone(),
            kind: kind.to_string(),
            cwd,
            agent,
            scrollback: None,
        });
    }
    finish_export(options, manifest, scrollbacks, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(output: PathBuf) -> ExportOptions {
        ExportOptions {
            output,
            force: false,
            include_agent_files: true,
            include_scrollback: true,
        }
    }

    #[test]
    fn persisted_export_copies_layout_and_agent_files() {
        let home = super::super::test_temp_dir("export-home");
        let roots = StoreRoots::for_home(home.clone());
        let app = home.join("app");
        std::fs::create_dir_all(&app).unwrap();
        let cwd = app.display().to_string();
        let transcript = roots
            .claude_dir
            .join("projects")
            .join(super::super::claude_project_key(&cwd))
            .join("session-1.jsonl");
        std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        std::fs::write(&transcript, "{\"a\":1}\n").unwrap();

        let session_dir = super::super::test_temp_dir("export-session");
        let session = serde_json::json!({
            "version": 3,
            "workspaces": [{
                "id": "w1",
                "identity_cwd": cwd,
                "public_pane_numbers": {"1": 1},
                "next_public_pane_number": 2,
                "public_tab_numbers": [1],
                "next_public_tab_number": 2,
                "tabs": [{
                    "layout": {"Pane": 1},
                    "panes": {"1": {
                        "cwd": cwd,
                        "agent_session": {
                            "source": "herdr:claude",
                            "agent": "claude",
                            "kind": "id",
                            "value": "session-1"
                        }
                    }},
                    "zoomed": false,
                    "focused": 1,
                    "root_pane": 1
                }],
                "active_tab": 0
            }],
            "active": 0,
            "selected": 0
        });
        std::fs::write(
            session_dir.join(SESSION_FILE),
            serde_json::to_string_pretty(&session).unwrap(),
        )
        .unwrap();

        let output = super::super::test_temp_dir("export-out");
        std::fs::remove_dir_all(&output).unwrap();
        let outcome = export_persisted(
            &session_dir,
            "default",
            Some(&roots),
            &[ScrollbackEntry {
                pane_id: "w1:p1".to_string(),
                text: "hello scrollback".to_string(),
            }],
            &options(output.clone()),
        )
        .unwrap();
        assert_eq!(outcome.panes, 1);
        assert_eq!(outcome.agents_copied, 1);
        assert!(output.join(SESSION_FILE).is_file());
        assert!(output.join("panes/w1-p1/scrollback.txt").is_file());
        let loaded = super::super::manifest::load_bundle(&output).unwrap();
        let super::super::manifest::LoadedBundle::V2(manifest) = loaded else {
            panic!("expected schema 2 bundle");
        };
        let agent = manifest.panes[0].agent.as_ref().unwrap();
        assert_eq!(
            agent.resume_argv.as_deref(),
            Some(
                [
                    "claude".to_string(),
                    "--resume".to_string(),
                    "session-1".to_string()
                ]
                .as_slice()
            )
        );
        assert!(agent.bundle_path.is_some());
        for dir in [home, session_dir, output] {
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn export_refuses_non_empty_output_without_force() {
        let session_dir = super::super::test_temp_dir("export-force-session");
        std::fs::write(
            session_dir.join(SESSION_FILE),
            r#"{"version":3,"workspaces":[],"active":null,"selected":0}"#,
        )
        .unwrap();
        let output = super::super::test_temp_dir("export-force-out");
        std::fs::write(output.join("keep.txt"), "keep").unwrap();
        let result = export_persisted(&session_dir, "default", None, &[], &options(output.clone()));
        assert!(result.is_err());
        assert_eq!(
            std::fs::read_to_string(output.join("keep.txt")).unwrap(),
            "keep"
        );
        for dir in [session_dir, output] {
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
}
