//! Bundle manifest (`manifest.json`) for schema 2, plus a read-only view of
//! schema 1 manifests written by the original bundle script.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{
    message_error, BundleError, BundleResult, BUNDLE_KIND, BUNDLE_SCHEMA_VERSION, MANIFEST_FILE,
    MIN_SUPPORTED_SCHEMA_VERSION,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Manifest {
    pub(crate) kind: String,
    pub(crate) schema_version: u32,
    /// Unix seconds when the bundle was created.
    pub(crate) created_unix: u64,
    pub(crate) source: SourceInfo,
    /// `persisted` when `session.json` is included, `remote_api` when only a
    /// live API snapshot could be captured (a `--machine` export).
    pub(crate) capture: String,
    #[serde(default)]
    pub(crate) panes: Vec<ManifestPane>,
    #[serde(default)]
    pub(crate) folders: BTreeMap<String, FolderInfo>,
    #[serde(default)]
    pub(crate) exclusions: Vec<String>,
    #[serde(default)]
    pub(crate) warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct SourceInfo {
    pub(crate) herdr_version: String,
    pub(crate) os: String,
    pub(crate) arch: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) home: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) session_name: Option<String>,
    /// Set for `--machine` exports: the saved machine label the snapshot was
    /// read from. The target was only read, never modified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) machine_label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ManifestPane {
    pub(crate) workspace: String,
    pub(crate) tab: String,
    /// Public pane ID (`w1:p1`) when captured from a live server; otherwise a
    /// positional label from the saved layout.
    pub(crate) pane: String,
    /// `agent`, `shell`, or `program`.
    pub(crate) kind: String,
    pub(crate) cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) agent: Option<ManifestAgent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) scrollback: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ManifestAgent {
    /// Herdr integration source such as `herdr:claude`, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) source: Option<String>,
    pub(crate) agent: String,
    /// `id` or `path`, matching `AgentSessionRefKind` serialization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) session_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) session_value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) resume_argv: Option<Vec<String>>,
    /// Copied session data, relative to the bundle root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) bundle_path: Option<String>,
    /// Why session files are absent, when they could not be copied:
    /// `not_found`, `unsupported_store`, `remote_not_transferred`, or
    /// `disabled`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) files_missing: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct FolderInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) git: Option<super::git_info::GitInfo>,
}

impl Manifest {
    pub(crate) fn new(source: SourceInfo, capture: &str) -> Self {
        Self {
            kind: BUNDLE_KIND.to_string(),
            schema_version: BUNDLE_SCHEMA_VERSION,
            created_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_secs())
                .unwrap_or(0),
            source,
            capture: capture.to_string(),
            panes: Vec::new(),
            folders: BTreeMap::new(),
            exclusions: vec![
                "agent credentials and logins (log in on each device)".to_string(),
                "project files (clone the repo; the git HEAD is recorded)".to_string(),
                "live process state, shell variables, and turns in flight".to_string(),
                "agent lock files, databases, logs, and tool-output spools".to_string(),
            ],
            warnings: Vec::new(),
        }
    }

    pub(crate) fn write_to(&self, bundle_dir: &Path) -> BundleResult<()> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|error| BundleError::Io(std::io::Error::other(error)))?;
        std::fs::write(bundle_dir.join(MANIFEST_FILE), json + "\n")?;
        Ok(())
    }
}

/// A bundle on disk, in whichever schema it was written.
pub(crate) enum LoadedBundle {
    V2(Box<Manifest>),
    V1(ManifestV1),
}

#[cfg(test)]
impl LoadedBundle {
    fn schema_version(&self) -> u32 {
        match self {
            Self::V2(manifest) => manifest.schema_version,
            Self::V1(manifest) => manifest.schema_version,
        }
    }
}

pub(crate) fn load_bundle(bundle_dir: &Path) -> BundleResult<LoadedBundle> {
    let manifest_path = bundle_dir.join(MANIFEST_FILE);
    let content = std::fs::read_to_string(&manifest_path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            message_error(format!(
                "{} is not a Herdr bundle (no {MANIFEST_FILE})",
                bundle_dir.display()
            ))
        } else {
            BundleError::Io(error)
        }
    })?;
    let probe: ManifestProbe = serde_json::from_str(&content)
        .map_err(|error| message_error(format!("invalid bundle manifest: {error}")))?;
    if probe.kind.as_deref() != Some(BUNDLE_KIND) {
        return Err(message_error(format!(
            "{} is not a Herdr bundle (unexpected kind)",
            bundle_dir.display()
        )));
    }
    let version = probe.schema_version.unwrap_or(0);
    if !(MIN_SUPPORTED_SCHEMA_VERSION..=BUNDLE_SCHEMA_VERSION).contains(&version) {
        return Err(message_error(format!(
            "bundle schema {version} is not supported (supported: {MIN_SUPPORTED_SCHEMA_VERSION}..={BUNDLE_SCHEMA_VERSION}); upgrade Herdr to import it"
        )));
    }
    if version == 1 {
        let manifest: ManifestV1 = serde_json::from_str(&content)
            .map_err(|error| message_error(format!("invalid schema 1 bundle manifest: {error}")))?;
        return Ok(LoadedBundle::V1(manifest));
    }
    let manifest: Manifest = serde_json::from_str(&content)
        .map_err(|error| message_error(format!("invalid bundle manifest: {error}")))?;
    Ok(LoadedBundle::V2(Box::new(manifest)))
}

#[derive(Deserialize)]
struct ManifestProbe {
    kind: Option<String>,
    schema_version: Option<u32>,
}

/// Schema 1 manifest, written by the original bundle script for the
/// `herdr-app`/`hc` runtime. Only the fields import needs are modelled; the
/// layout tree is rebuilt from the flat pane list.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub(crate) struct ManifestV1 {
    pub(crate) schema_version: u32,
    #[serde(default)]
    pub(crate) home: Option<String>,
    #[serde(default)]
    pub(crate) panes: Vec<PaneV1>,
    #[serde(default)]
    pub(crate) folders: BTreeMap<String, FolderV1>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub(crate) struct PaneV1 {
    pub(crate) address: String,
    #[serde(default)]
    pub(crate) space: Option<String>,
    #[serde(default)]
    pub(crate) tab: Option<String>,
    pub(crate) kind: String,
    pub(crate) cwd: String,
    #[serde(default)]
    pub(crate) agent: Option<AgentV1>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub(crate) struct AgentV1 {
    pub(crate) name: String,
    pub(crate) session_id: String,
    #[serde(default)]
    pub(crate) store_relpath: Option<String>,
    #[serde(default)]
    pub(crate) bundle_path: Option<String>,
    #[serde(default)]
    pub(crate) resume: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub(crate) struct FolderV1 {
    #[serde(default)]
    pub(crate) git: Option<super::git_info::GitInfo>,
}

pub(crate) fn render_readme(manifest: &Manifest) -> String {
    let mut lines = vec![
        format!("# Herdr bundle (schema {})", manifest.schema_version),
        String::new(),
        format!(
            "Created by Herdr {} on {} ({}). Capture: {}.",
            manifest.source.herdr_version,
            manifest.source.os,
            manifest.source.arch,
            manifest.capture
        ),
        "Restore with: `herdr session import <this-folder>`".to_string(),
        "Contains full agent transcripts. Keep private; do not commit or share.".to_string(),
        String::new(),
        "## Panes".to_string(),
    ];
    for pane in &manifest.panes {
        let resume = pane
            .agent
            .as_ref()
            .and_then(|agent| agent.resume_argv.as_ref())
            .map(|argv| format!(" -> `{}`", argv.join(" ")))
            .unwrap_or_default();
        lines.push(format!(
            "- {} [{}] {}{resume}",
            pane.pane, pane.kind, pane.cwd
        ));
    }
    if !manifest.warnings.is_empty() {
        lines.push(String::new());
        lines.push("## Warnings".to_string());
        for warning in &manifest.warnings {
            lines.push(format!("- {warning}"));
        }
    }
    lines.push(String::new());
    lines.push("## Not included".to_string());
    for exclusion in &manifest.exclusions {
        lines.push(format!("- {exclusion}"));
    }
    lines.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_round_trips() {
        let dir = super::super::test_temp_dir("manifest");
        let mut manifest = Manifest::new(
            SourceInfo {
                herdr_version: "0.9.5".to_string(),
                os: "linux".to_string(),
                arch: "x86_64".to_string(),
                home: Some("/home/ada".to_string()),
                session_name: Some("default".to_string()),
                machine_label: None,
            },
            "persisted",
        );
        manifest.panes.push(ManifestPane {
            workspace: "w1".to_string(),
            tab: "w1:t1".to_string(),
            pane: "w1:p1".to_string(),
            kind: "agent".to_string(),
            cwd: "/home/ada/app".to_string(),
            agent: Some(ManifestAgent {
                source: Some("herdr:claude".to_string()),
                agent: "claude".to_string(),
                session_kind: Some("id".to_string()),
                session_value: Some("session-1".to_string()),
                resume_argv: Some(vec![
                    "claude".to_string(),
                    "--resume".to_string(),
                    "session-1".to_string(),
                ]),
                bundle_path: Some("agents/claude/-home-ada-app/session-1.jsonl".to_string()),
                files_missing: None,
            }),
            scrollback: None,
        });
        manifest.write_to(&dir).unwrap();
        let loaded = load_bundle(&dir).unwrap();
        assert_eq!(loaded.schema_version(), 2);
        let LoadedBundle::V2(loaded) = loaded else {
            panic!("expected schema 2 bundle");
        };
        assert_eq!(*loaded, manifest);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn schema_one_manifest_loads() {
        let dir = super::super::test_temp_dir("manifest-v1");
        std::fs::write(
            dir.join(MANIFEST_FILE),
            r#"{"kind":"herdr-bundle","schema_version":1,"home":"/home/ada","panes":[{"address":"t1","space":"sp1","tab":"tb1","kind":"agent","cwd":"/home/ada/app","agent":{"name":"muse","session_id":"session-1","store_relpath":"2026/11/07","bundle_path":"agents/muse/2026/11/07/session-1","resume":["muse","resume","session-1"]}}],"folders":{}}"#,
        )
        .unwrap();
        let loaded = load_bundle(&dir).unwrap();
        let LoadedBundle::V1(manifest) = loaded else {
            panic!("expected schema 1 bundle");
        };
        assert_eq!(manifest.panes.len(), 1);
        assert_eq!(manifest.panes[0].agent.as_ref().unwrap().name, "muse");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn non_bundle_and_newer_schema_are_rejected() {
        let dir = super::super::test_temp_dir("manifest-reject");
        assert!(load_bundle(&dir).is_err());
        std::fs::write(
            dir.join(MANIFEST_FILE),
            r#"{"kind":"herdr-bundle","schema_version":99}"#,
        )
        .unwrap();
        let error = load_bundle(&dir)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(error.contains("not supported"), "{error}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
