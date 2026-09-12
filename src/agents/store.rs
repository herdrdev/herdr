//! Transactional, session-local registry snapshots. Candidate compilation and
//! durable publication finish before the active Arc is changed. No startup writes.
//!
//! The journal is a last-known-good *package source*, not an archive of mutable
//! detection overrides or remote cache files. Restart recompiles its exact source
//! bytes against the currently valid detection overlays. Initialization alone
//! never establishes a durable LKG; a successful reload publication does.

use std::fs;
use std::io::Write;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, RwLock};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{bundled, files, remote, AgentRegistry};
use crate::detect::{manifest, Agent};

type SourceFiles = Vec<(String, String)>;
const MAX_ERROR_BYTES: usize = 2048;
const MAX_SOURCE_PATH_BYTES: usize = 4096;
const MAX_REMOTE_HISTORY: usize = 64;
// JSON can expand one source byte to six bytes. File paths and metadata have
// independent bounds; this cap is checked before deserializing any journal.
const MAX_JOURNAL_BYTES: u64 = files::MAX_TOTAL_BYTES * 6 + 4 * 1024 * 1024;

#[derive(Debug)]
pub(crate) struct RegistrySnapshot {
    pub(crate) registry: Arc<AgentRegistry>,
    pub(crate) manifests: manifest::ManifestCache,
    pub(crate) generation: u64,
    pub(crate) digest: String,
    pub(crate) source: Option<PathBuf>,
    pub(crate) remote: Option<remote::RemoteRevision>,
    accepted_remote: Vec<remote::RemoteRevision>,
    files: Arc<SourceFiles>,
}

impl Deref for RegistrySnapshot {
    type Target = AgentRegistry;

    fn deref(&self) -> &Self::Target {
        &self.registry
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RegistryStatus {
    pub(crate) generation: u64,
    pub(crate) digest: String,
    /// Local source directory; None selects the managed bundled/R2 source.
    pub(crate) source: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) remote: Option<remote::RemoteRevision>,
    pub(crate) last_error: Option<String>,
    pub(crate) agents: Vec<AgentSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RegistryUpdateCheck {
    pub(crate) active_generation: u64,
    pub(crate) active_digest: String,
    pub(crate) remote: remote::RemoteRevision,
    pub(crate) content_sha256: String,
    pub(crate) update_available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentSummary {
    pub(crate) id: String,
    pub(crate) startable: bool,
    pub(crate) process: bool,
    pub(crate) detection: bool,
    pub(crate) resume: bool,
    pub(crate) integration: bool,
}

pub(crate) struct RegistryStore {
    active: RwLock<Arc<RegistrySnapshot>>,
    generation: AtomicU64,
    reload_lock: Mutex<()>,
    download_lock: Mutex<()>,
    configured_source: Mutex<Option<PathBuf>>,
    last_error: Mutex<Option<String>>,
    journal_path: Option<PathBuf>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    schema: u32,
    generation: u64,
    digest: String,
    source: Option<PathBuf>,
    // Selection survives even when the active bytes must fall back to bundled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    selected_source: Option<PathBuf>,
    #[serde(default)]
    remote: Option<remote::RemoteRevision>,
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "deserialize_remote_history"
    )]
    accepted_remote: Vec<remote::RemoteRevision>,
    #[serde(deserialize_with = "deserialize_journal_files")]
    files: Vec<JournalFile>,
}

impl Journal {
    fn into_snapshot(self) -> Result<RegistrySnapshot, String> {
        // Integrity-valid control metadata does not authorize incompatible packages.
        let files = self
            .files
            .into_iter()
            .map(|file| (file.path, file.content))
            .collect();
        let mut snapshot = build_snapshot(files, self.source, self.generation)?;
        snapshot.remote = self.remote;
        snapshot.accepted_remote = self.accepted_remote;
        manifest::apply_registry_provenance(
            &mut snapshot.manifests,
            snapshot.source.as_deref(),
            snapshot.remote.as_ref(),
        );
        Ok(snapshot)
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JournalFile {
    path: String,
    content: String,
    sha256: String,
}

fn deserialize_remote_history<'de, D>(
    deserializer: D,
) -> Result<Vec<remote::RemoteRevision>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct Visitor;
    impl<'de> serde::de::Visitor<'de> for Visitor {
        type Value = Vec<remote::RemoteRevision>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("bounded accepted registry publications")
        }
        fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
        where
            A: serde::de::SeqAccess<'de>,
        {
            let mut entries = Vec::new();
            while let Some(entry) = sequence.next_element::<remote::RemoteRevision>()? {
                if entries.len() >= MAX_REMOTE_HISTORY {
                    return Err(serde::de::Error::custom(
                        "too many accepted registry origins/channels",
                    ));
                }
                entry.validate().map_err(serde::de::Error::custom)?;
                if entries
                    .iter()
                    .any(|old: &remote::RemoteRevision| same_channel(old, &entry))
                {
                    return Err(serde::de::Error::custom(
                        "duplicate accepted registry origin/channel",
                    ));
                }
                entries.push(entry);
            }
            Ok(entries)
        }
    }
    deserializer.deserialize_seq(Visitor)
}

fn same_channel(left: &remote::RemoteRevision, right: &remote::RemoteRevision) -> bool {
    left.origin == right.origin && left.pointer.channel == right.pointer.channel
}

// Bound the collection while decoding, not after an attacker-controlled JSON
// array has allocated millions of empty records.
fn deserialize_journal_files<'de, D>(deserializer: D) -> Result<Vec<JournalFile>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct BoundedFiles;
    impl<'de> serde::de::Visitor<'de> for BoundedFiles {
        type Value = Vec<JournalFile>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a bounded source file array")
        }
        fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
        where
            A: serde::de::SeqAccess<'de>,
        {
            let mut files = Vec::new();
            let mut total = 0u64;
            while let Some(file) = sequence.next_element::<JournalFile>()? {
                total += file.content.len() as u64;
                if files.len() >= files::MAX_FILES
                    || file.path.len() > 256
                    || file.content.len() as u64 > files::MAX_FILE_BYTES
                    || total > files::MAX_TOTAL_BYTES
                {
                    return Err(serde::de::Error::custom(
                        "registry journal source exceeds limits",
                    ));
                }
                files.push(file);
            }
            Ok(files)
        }
    }
    deserializer.deserialize_seq(BoundedFiles)
}

impl RegistryStore {
    /// An isolated store with caller-owned persistence; does not write or consult
    /// the global registry. Useful for embedders and deterministic local tests.
    #[cfg(test)]
    pub(crate) fn new(files: SourceFiles, journal_path: PathBuf) -> Result<Self, String> {
        Ok(Self::from_snapshot(
            build_snapshot(files, None, 1)?,
            None,
            journal_path,
            None,
        ))
    }

    fn from_snapshot(
        snapshot: RegistrySnapshot,
        configured_source: Option<PathBuf>,
        journal_path: PathBuf,
        last_error: Option<String>,
    ) -> Self {
        Self {
            generation: AtomicU64::new(snapshot.generation),
            active: RwLock::new(Arc::new(snapshot)),
            reload_lock: Mutex::new(()),
            download_lock: Mutex::new(()),
            configured_source: Mutex::new(configured_source),
            last_error: Mutex::new(last_error),
            journal_path: Some(journal_path),
        }
    }

    fn startup(mut configured: Option<PathBuf>, journal_path: PathBuf) -> Self {
        let mut errors = Vec::new();
        let mut accepted_remote = Vec::new();
        match read_journal(&journal_path) {
            Ok(Some(journal)) => {
                configured = configured.or_else(|| journal.selected_source.clone());
                accepted_remote = journal.accepted_remote.clone();
                let matches_source = configured.as_ref().is_none_or(|selected| {
                    journal
                        .source
                        .as_ref()
                        .is_some_and(|saved| source_identity(selected) == source_identity(saved))
                });
                if matches_source {
                    match journal.into_snapshot() {
                        Ok(snapshot) => {
                            return Self::from_snapshot(snapshot, configured, journal_path, None)
                        }
                        Err(error) => {
                            errors.push(format!("last-known-good registry rejected: {error}"))
                        }
                    }
                } else if configured.as_deref().map(source_identity)
                    != journal.selected_source.as_deref().map(source_identity)
                {
                    errors.push("saved registry belongs to a different selected source".into());
                }
            }
            Ok(None) => {}
            Err(error) => errors.push(format!("last-known-good registry rejected: {error}")),
        }
        if let Some(source) = &configured {
            match read_candidate(Some(source), 1) {
                Ok(mut snapshot) => {
                    snapshot.accepted_remote = accepted_remote;
                    let error = startup_error(errors);
                    let source = snapshot.source.clone();
                    return Self::from_snapshot(snapshot, source, journal_path, error);
                }
                Err(error) => errors.push(format!(
                    "configured registry rejected; using bundled source: {error}"
                )),
            }
        }
        let mut snapshot = read_candidate(None, 1).unwrap_or_else(|error| {
            // A bad external source must never disable the trusted fallback.
            // Invalid compiled-in data is a build defect, not a reload failure.
            panic!("bundled agent registry is invalid: {error}")
        });
        snapshot.accepted_remote = accepted_remote;
        let error = startup_error(errors);
        Self::from_snapshot(snapshot, configured, journal_path, error)
    }

    pub(crate) fn snapshot(&self) -> Arc<RegistrySnapshot> {
        self.active
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn persist(
        &self,
        snapshot: &RegistrySnapshot,
        selected_source: Option<&Path>,
    ) -> Result<(), String> {
        match &self.journal_path {
            Some(path) => persist(path, snapshot, selected_source),
            None => Ok(()), // only the global unit-test store is nonpersistent
        }
    }

    fn publish(&self, candidate: RegistrySnapshot) {
        let mut active = self.active.write().unwrap_or_else(|e| e.into_inner());
        let generation = candidate.generation;
        *active = Arc::new(candidate);
        self.generation.store(generation, Ordering::Release);
    }

    pub(crate) fn status(&self) -> RegistryStatus {
        let active = self.snapshot();
        RegistryStatus {
            generation: active.generation,
            digest: active.digest.clone(),
            source: active.source.clone(),
            remote: active.remote.clone(),
            agents: active
                .known_profiles()
                .map(|profile| AgentSummary {
                    id: profile.canonical_id().to_owned(),
                    startable: profile.is_startable(),
                    process: profile.process().is_some(),
                    detection: profile.is_screen_detectable(),
                    resume: profile.session().is_some(),
                    integration: profile.integration().is_some(),
                })
                .collect(),
            last_error: self
                .last_error
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
        }
    }

    fn outcome<T>(&self, result: Result<T, String>) -> Result<T, String> {
        let result = result.map_err(bounded_error);
        *self.last_error.lock().unwrap_or_else(|e| e.into_inner()) = result.as_ref().err().cloned();
        result
    }

    fn reload_guard(&self) -> Result<std::sync::MutexGuard<'_, ()>, String> {
        match self.reload_lock.try_lock() {
            Ok(guard) => Ok(guard),
            Err(_) => self.outcome(Err("agent registry reload busy".into())),
        }
    }

    pub(crate) fn reload(&self, source: Option<&Path>) -> Result<RegistryStatus, String> {
        let _guard = self.reload_guard()?;
        let result = (|| {
            let retained = self
                .configured_source
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            let source = source.or(retained.as_deref());
            let active = self.snapshot();
            let mut candidate = if source.is_none() && active.remote.is_some() {
                let mut candidate =
                    build_snapshot((*active.files).clone(), None, active.generation)?;
                candidate.remote = active.remote.clone();
                candidate
            } else {
                read_candidate(source, active.generation)?
            };
            validate_integration_revisions(&active, &candidate)?;
            candidate.accepted_remote = active.accepted_remote.clone();
            if candidate.digest != active.digest {
                candidate.generation = next_generation(active.generation)?;
            } else {
                // Byte-identical reloads must not silently refresh detection under
                // an unchanged generation. Detection updates have their own path.
                candidate.registry = active.registry.clone();
                candidate.manifests = active.manifests.clone();
            }
            manifest::apply_registry_provenance(
                &mut candidate.manifests,
                candidate.source.as_deref(),
                candidate.remote.as_ref(),
            );
            self.persist(&candidate, candidate.source.as_deref())?;
            *self
                .configured_source
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = candidate.source.clone();
            self.publish(candidate);
            Ok(())
        })();
        self.outcome(result)?;
        Ok(self.status())
    }

    pub(crate) fn reset(&self) -> Result<RegistryStatus, String> {
        let _guard = self.reload_guard()?;
        self.outcome((|| {
            let active = self.snapshot();
            let mut candidate = read_candidate(None, next_generation(active.generation)?)?;
            candidate.accepted_remote = active.accepted_remote.clone();
            self.persist(&candidate, None)?;
            *self
                .configured_source
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = None;
            self.publish(candidate);
            Ok(())
        })())?;
        Ok(self.status())
    }

    fn prepare_remote(
        &self,
        channel: Option<remote::Channel>,
    ) -> Result<(Arc<RegistrySnapshot>, RegistrySnapshot), String> {
        let _download = self
            .download_lock
            .try_lock()
            .map_err(|_| "registry download already in progress")?;
        let active = self.snapshot();
        self.require_managed_source(&active)?;
        let channel = channel.unwrap_or_else(|| {
            active
                .remote
                .as_ref()
                .map(|revision| revision.pointer.channel)
                .unwrap_or_default()
        });
        let (revision, verified) = remote::download(&remote::origin()?, channel)?;
        let candidate = remote_candidate(&active, revision, verified)?;
        Ok((active, candidate))
    }

    fn require_managed_source(&self, snapshot: &RegistrySnapshot) -> Result<(), String> {
        if snapshot.source.is_some()
            || self
                .configured_source
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some()
        {
            return Err("a local registry is selected; use registry reset before updating from R2 (local files are not modified)".into());
        }
        Ok(())
    }

    fn automatic_update_channel(&self) -> Option<remote::Channel> {
        let active = self.snapshot();
        self.require_managed_source(&active).ok()?;
        Some(
            active
                .remote
                .as_ref()
                .map(|revision| revision.pointer.channel)
                .unwrap_or_default(),
        )
    }

    pub(crate) fn check_remote(
        &self,
        channel: remote::Channel,
    ) -> Result<RegistryUpdateCheck, String> {
        self.outcome((|| {
            let (active, candidate) = self.prepare_remote(Some(channel))?;
            // A check is advisory and must never publish or write a journal.
            let remote = candidate
                .remote
                .clone()
                .ok_or("missing remote registry provenance")?;
            Ok(RegistryUpdateCheck {
                active_generation: active.generation,
                active_digest: active.digest.clone(),
                update_available: candidate.digest != active.digest,
                content_sha256: candidate.digest,
                remote,
            })
        })())
    }

    fn update_remote(&self, channel: Option<remote::Channel>) -> Result<RegistryStatus, String> {
        let result = (|| {
            let (expected, candidate) = self.prepare_remote(channel)?;
            self.activate_remote(&expected, candidate)
        })();
        self.outcome(result)?;
        Ok(self.status())
    }

    fn activate_remote(
        &self,
        expected: &RegistrySnapshot,
        mut candidate: RegistrySnapshot,
    ) -> Result<(), String> {
        let _guard = self.reload_guard()?;
        let active = self.snapshot();
        self.require_managed_source(&active)?;
        if active.generation != expected.generation
            || active.source != expected.source
            || active.remote != expected.remote
            || active.accepted_remote != expected.accepted_remote
        {
            return Err("registry changed during download; retry the update".into());
        }
        validate_integration_revisions(&active, &candidate)?;
        if active.digest == candidate.digest {
            candidate.registry = active.registry.clone();
            candidate.manifests = active.manifests.clone();
        }
        manifest::apply_registry_provenance(
            &mut candidate.manifests,
            None,
            candidate.remote.as_ref(),
        );
        self.persist(&candidate, None)?;
        self.publish(candidate);
        Ok(())
    }

    fn replace_detection(
        &self,
        builder: impl FnOnce(&RegistrySnapshot) -> manifest::ManifestCache,
    ) -> Result<(), String> {
        let _guard = self.reload_guard()?;
        self.outcome((|| {
            let active = self.snapshot();
            let mut candidate = RegistrySnapshot {
                registry: active.registry.clone(),
                manifests: builder(&active),
                generation: next_generation(active.generation)?,
                digest: active.digest.clone(),
                source: active.source.clone(),
                remote: active.remote.clone(),
                accepted_remote: active.accepted_remote.clone(),
                files: active.files.clone(),
            };
            manifest::apply_registry_provenance(
                &mut candidate.manifests,
                candidate.source.as_deref(),
                candidate.remote.as_ref(),
            );
            let selected = self
                .configured_source
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            self.persist(&candidate, selected.as_deref())?;
            self.publish(candidate);
            Ok(())
        })())
    }
}

fn remote_candidate(
    active: &RegistrySnapshot,
    revision: remote::RemoteRevision,
    verified: remote::VerifiedSnapshot,
) -> Result<RegistrySnapshot, String> {
    revision.validate()?;
    let mut accepted_remote = active.accepted_remote.clone();
    if let Some(previous) = accepted_remote
        .iter_mut()
        .find(|old| same_channel(old, &revision))
    {
        if revision.pointer.generation < previous.pointer.generation {
            return Err(
                "registry publication is older than the accepted channel generation".into(),
            );
        }
        if revision.pointer.generation == previous.pointer.generation
            && revision.pointer != previous.pointer
        {
            return Err(
                "registry channel generation was reused for different snapshot bytes".into(),
            );
        }
        *previous = revision.clone();
    } else {
        if accepted_remote.len() >= MAX_REMOTE_HISTORY {
            return Err("accepted registry origin/channel history is full".into());
        }
        accepted_remote.push(revision.clone());
    }
    let generation = if verified.content_sha256 == active.digest {
        active.generation
    } else {
        next_generation(active.generation)?
    };
    let mut candidate = build_snapshot(verified.files, None, generation)?;
    validate_integration_revisions(active, &candidate)?;
    if candidate.digest != verified.content_sha256 || revision.commit != verified.commit {
        return Err("registry snapshot provenance/content changed before activation".into());
    }
    candidate.remote = Some(revision);
    candidate.accepted_remote = accepted_remote;
    manifest::apply_registry_provenance(&mut candidate.manifests, None, candidate.remote.as_ref());
    Ok(candidate)
}

fn next_generation(generation: u64) -> Result<u64, String> {
    generation
        .checked_add(1)
        .ok_or_else(|| "registry generation exhausted".into())
}

fn bounded_error(mut error: String) -> String {
    if error.len() > MAX_ERROR_BYTES {
        let mut end = MAX_ERROR_BYTES - 3;
        while !error.is_char_boundary(end) {
            end -= 1;
        }
        error.truncate(end);
        error.push_str("...");
    }
    error
}

fn startup_error(errors: Vec<String>) -> Option<String> {
    if errors.is_empty() {
        return None;
    }
    let error = bounded_error(errors.join("; "));
    eprintln!("agent registry: {error}");
    tracing::error!(%error, "agent registry startup fallback");
    Some(error)
}

fn source_identity(source: &Path) -> PathBuf {
    let absolute = if source.is_absolute() {
        source.to_owned()
    } else {
        std::env::current_dir().unwrap_or_default().join(source)
    };
    for ancestor in absolute.ancestors() {
        if let Ok(canonical) = fs::canonicalize(ancestor) {
            if let Ok(suffix) = absolute.strip_prefix(ancestor) {
                // A deleted source must retain its canonical parent spelling, including
                // Windows' verbatim prefix, to match the saved last-good source.
                return canonical.join(suffix);
            }
        }
    }
    absolute
}

fn check_source_path(source: &Path) -> Result<(), String> {
    match source.to_str() {
        Some(value) if !value.is_empty() && value.len() <= MAX_SOURCE_PATH_BYTES => Ok(()),
        _ => Err("registry source path must be bounded nonempty UTF-8".into()),
    }
}

fn read_candidate(source: Option<&Path>, generation: u64) -> Result<RegistrySnapshot, String> {
    let (files, source) = match source {
        Some(source) => {
            check_source_path(source)?;
            let files = files::read_source(source)?;
            // Read before canonicalization so a symlink root cannot bypass the reader.
            let path = fs::canonicalize(source).map_err(|e| e.to_string())?;
            check_source_path(&path)?;
            (files, Some(path))
        }
        None => (
            bundled::FILES
                .iter()
                .map(|(p, t)| ((*p).into(), (*t).into()))
                .collect(),
            None,
        ),
    };
    build_snapshot(files, source, generation)
}

fn build_snapshot(
    mut files: SourceFiles,
    source: Option<PathBuf>,
    generation: u64,
) -> Result<RegistrySnapshot, String> {
    files.sort_by(|left, right| left.0.cmp(&right.0));
    let borrowed: Vec<_> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    let packages = super::validate_packages(&borrowed)?;
    validate_integration_baseline(&packages)?;
    let registry = Arc::new(AgentRegistry::from_packages(packages)?);
    let mut manifests = manifest::build_manifest_cache(&registry);
    manifest::apply_registry_provenance(&mut manifests, source.as_deref(), None);
    Ok(RegistrySnapshot {
        registry,
        manifests,
        generation,
        digest: source_digest(&files),
        source,
        remote: None,
        accepted_remote: Vec::new(),
        files: Arc::new(files),
    })
}

// The compiled installer layout is validated separately. Its bundled version is
// the compatibility floor; newer versioned payloads use that same installer.
pub(crate) fn validate_integration_baseline(
    packages: &[super::source::Package],
) -> Result<(), String> {
    for package in packages {
        let Some(integration) = &package.integration else {
            continue;
        };
        let id = &package.identity.id;
        if crate::integration::builtin::binding(Agent::parse(id)?).is_none() {
            continue;
        }
        for asset in &integration.assets {
            if asset.role == "manifest" {
                continue;
            }
            let text = package
                .assets
                .get(&asset.path)
                .ok_or_else(|| format!("{id}: missing integration asset {}", asset.path))?;
            if super::source::markers(text, "HERDR_INTEGRATION_ID=").len() != 1
                || super::source::markers(text, "HERDR_INTEGRATION_VERSION=").len() != 1
            {
                return Err(format!(
                    "{id}: integration asset {} requires identity and version markers",
                    asset.path
                ));
            }
        }
        let metadata_path = format!("agents/{id}/integration.toml");
        let metadata = bundled::FILES
            .iter()
            .find(|(path, _)| *path == metadata_path)
            .map(|(_, text)| *text)
            .ok_or_else(|| format!("missing bundled integration definition for {id}"))?;
        let baseline: super::source::IntegrationDefinition = toml::from_str(metadata)
            .map_err(|error| format!("invalid bundled integration {id}: {error}"))?;
        for (platform, version, minimum) in [
            ("unix", integration.versions.unix, baseline.versions.unix),
            (
                "windows",
                integration.versions.windows,
                baseline.versions.windows,
            ),
        ] {
            if version < minimum {
                return Err(format!("{id}: integration version {version} on {platform} is older than this installer's minimum {minimum}"));
            }
            if version == minimum {
                validate_same_version_assets(id, platform, integration, |path| {
                    let full_path = format!("agents/{id}/{path}");
                    let baseline = bundled::FILES
                        .iter()
                        .find(|(name, _)| *name == full_path)
                        .map(|(_, text)| *text);
                    package.assets.get(path).map(String::as_str) == baseline
                })?;
            }
        }
    }
    Ok(())
}

fn validate_same_version_assets(
    id: &str,
    platform: &str,
    definition: &super::source::IntegrationDefinition,
    unchanged: impl Fn(&str) -> bool,
) -> Result<(), String> {
    for asset in &definition.assets {
        if (asset.platform == "all" || asset.platform == platform) && !unchanged(&asset.path) {
            return Err(format!(
                "{id}: integration asset {} changed on {platform} without changing its version",
                asset.path
            ));
        }
    }
    Ok(())
}

fn validate_integration_revisions(
    previous: &AgentRegistry,
    candidate: &AgentRegistry,
) -> Result<(), String> {
    for profile in candidate.integration_capable_profiles() {
        let Some(next) = profile.integration() else {
            continue;
        };
        let Some(old) = previous
            .profile_by_id(profile.canonical_id())
            .and_then(|profile| profile.integration())
        else {
            continue;
        };
        for (platform, version, previous_version) in [
            (
                "unix",
                next.definition.versions.unix,
                old.definition.versions.unix,
            ),
            (
                "windows",
                next.definition.versions.windows,
                old.definition.versions.windows,
            ),
        ] {
            if version == previous_version {
                validate_same_version_assets(
                    profile.canonical_id(),
                    platform,
                    &next.definition,
                    |path| next.assets.get(path) == old.assets.get(path),
                )?;
            }
        }
    }
    Ok(())
}

fn hash(content: &str) -> String {
    format!("{:x}", Sha256::digest(content.as_bytes()))
}

// Same sorted exact-byte content pin as scripts/agent_registry_vendor.py.
fn source_digest(files: &SourceFiles) -> String {
    source_digest_entries(
        files
            .iter()
            .map(|(path, content)| (path.as_str(), content.as_str())),
    )
}

fn source_digest_entries<'a>(files: impl Iterator<Item = (&'a str, &'a str)>) -> String {
    let mut digest = Sha256::new();
    for (path, content) in files {
        digest.update(format!("{}  {path}\n", hash(content)).as_bytes());
    }
    format!("{:x}", digest.finalize())
}

fn read_journal(path: &Path) -> Result<Option<Journal>, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > MAX_JOURNAL_BYTES
    {
        return Err("registry journal must be a bounded regular file".into());
    }
    let text = files::read_capped(files::open_regular(path)?, MAX_JOURNAL_BYTES)?;
    let mut journal: Journal = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    if journal.schema != 1 || journal.generation == 0 || journal.files.len() > files::MAX_FILES {
        return Err("invalid registry journal schema, generation or file count".into());
    }
    if journal.selected_source.is_none() {
        journal.selected_source = journal.source.clone();
    }
    if let Some(selected) = &journal.selected_source {
        check_source_path(selected)?;
    }
    if let Some(source) = &journal.source {
        check_source_path(source)?;
        if journal
            .selected_source
            .as_ref()
            .is_none_or(|selected| source_identity(selected) != source_identity(source))
        {
            return Err("registry journal local source does not match selection".into());
        }
    }
    let mut total = 0u64;
    for file in &journal.files {
        total += file.content.len() as u64;
        if file.path.len() > 256
            || file.content.len() as u64 > files::MAX_FILE_BYTES
            || total > files::MAX_TOTAL_BYTES
        {
            return Err("registry journal source exceeds limits".into());
        }
        if file.sha256 != hash(&file.content) {
            return Err("registry journal file hash mismatch".into());
        }
    }
    journal.files.sort_by(|a, b| a.path.cmp(&b.path));
    if journal.digest
        != source_digest_entries(
            journal
                .files
                .iter()
                .map(|file| (file.path.as_str(), file.content.as_str())),
        )
    {
        return Err("registry journal digest mismatch".into());
    }
    if let Some(remote) = &journal.remote {
        remote.validate()?;
        if journal.selected_source.is_some() {
            return Err("registry journal cannot select local and remote sources together".into());
        }
        if journal.accepted_remote.is_empty() {
            journal.accepted_remote.push(remote.clone());
        } else if !journal
            .accepted_remote
            .iter()
            .any(|accepted| accepted == remote)
        {
            return Err("active registry publication does not match accepted history".into());
        }
    }
    Ok(Some(journal))
}

fn persist(
    path: &Path,
    snapshot: &RegistrySnapshot,
    selected_source: Option<&Path>,
) -> Result<(), String> {
    let journal = Journal {
        schema: 1,
        generation: snapshot.generation,
        digest: snapshot.digest.clone(),
        source: snapshot.source.clone(),
        selected_source: selected_source.map(Path::to_path_buf),
        remote: snapshot.remote.clone(),
        accepted_remote: snapshot.accepted_remote.clone(),
        files: snapshot
            .files
            .iter()
            .map(|(path, content)| JournalFile {
                path: path.clone(),
                content: content.clone(),
                sha256: hash(content),
            })
            .collect(),
    };
    let bytes = serde_json::to_vec(&journal).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_JOURNAL_BYTES {
        return Err("registry journal exceeds limit".into());
    }
    atomic_write(path, &bytes)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().ok_or("registry journal has no parent")?;
    fs::create_dir_all(parent).map_err(|e| format!("create registry journal directory: {e}"))?;
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
            return Err("registry journal is not a regular file".into())
        }
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.to_string()),
        _ => {}
    }
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let temp = parent.join(format!(
        ".active.{}.{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = crate::platform::create_private_file(&temp)
        .map_err(|e| format!("create registry journal: {e}"))?;
    let result = file.write_all(bytes).and_then(|_| file.sync_all());
    drop(file);
    if let Err(error) = result.and_then(|_| fs::rename(&temp, path)) {
        let _ = fs::remove_file(&temp);
        return Err(format!("persist registry journal: {error}"));
    }
    // Rename is the commit point. A subsequent directory-sync failure cannot be
    // reported as rollback: the committed journal and active snapshot must agree.
    if let Err(error) = crate::platform::sync_directory_after_replace(parent) {
        tracing::warn!(%error, "registry committed but journal directory sync failed");
    }
    Ok(())
}

#[cfg(not(test))]
static STORE: LazyLock<RegistryStore> = LazyLock::new(|| {
    RegistryStore::startup(
        std::env::var_os("HERDR_AGENT_REGISTRY_SOURCE").map(PathBuf::from),
        crate::session::data_dir().join("agent-registry/active.json"),
    )
});

// Existing detection unit tests change XDG directories around global refreshes.
// They must neither read nor overwrite a real session journal. Filesystem and
// restart tests below always construct isolated, genuinely persistent stores.
#[cfg(test)]
static STORE: LazyLock<RegistryStore> = LazyLock::new(|| {
    let mut store = RegistryStore::from_snapshot(
        read_candidate(None, 1).expect("valid bundled registry"),
        None,
        PathBuf::new(),
        None,
    );
    store.journal_path = None;
    store
});

#[cfg(test)]
pub(crate) fn snapshot_for_test(
    files: SourceFiles,
    generation: u64,
) -> Result<Arc<RegistrySnapshot>, String> {
    build_snapshot(files, None, generation).map(Arc::new)
}

pub(crate) fn snapshot() -> Arc<RegistrySnapshot> {
    STORE.snapshot()
}
pub(crate) fn generation() -> u64 {
    STORE.generation.load(Ordering::Acquire)
}
pub(crate) fn status() -> RegistryStatus {
    STORE.status()
}
pub(crate) fn reload(source: Option<&Path>) -> Result<RegistryStatus, String> {
    STORE.reload(source)
}
pub(crate) fn reset() -> Result<RegistryStatus, String> {
    STORE.reset()
}
pub(crate) fn check_remote(channel: remote::Channel) -> Result<RegistryUpdateCheck, String> {
    STORE.check_remote(channel)
}
pub(crate) fn update_remote(channel: remote::Channel) -> Result<RegistryStatus, String> {
    STORE.update_remote(Some(channel))
}
pub(crate) fn auto_update_remote() -> Result<Option<RegistryStatus>, String> {
    if STORE.automatic_update_channel().is_none() {
        return Ok(None);
    }
    // Resolve the channel from the download's snapshot, not this preflight.
    STORE.update_remote(None).map(Some)
}
pub(crate) fn replace_detection(
    builder: impl FnOnce(&RegistrySnapshot) -> manifest::ManifestCache,
) -> Result<(), String> {
    STORE.replace_detection(builder)
}
pub(crate) fn refresh_detection(agents: Option<&[Agent]>) -> Vec<manifest::AgentManifestSummary> {
    if let Err(error) = replace_detection(|snapshot| match agents {
        Some(agents) => manifest::build_manifest_cache_for_agents(
            &snapshot.registry,
            &snapshot.manifests,
            agents,
        ),
        None => manifest::build_manifest_cache(&snapshot.registry),
    }) {
        tracing::warn!(%error, "agent detection refresh rejected; retaining active snapshot");
    }
    manifest::summaries(&snapshot().manifests)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    const AGENT: &str = "schema = 1\nid = 'future-agent'\nname = 'Future agent'\naliases = []\nstartable = true\n[launch]\nunix = 'future-agent'\nwindows = 'future-agent.cmd'\n";
    const PATH: &str = "agents/future-agent/agent.toml";

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            #[cfg(unix)]
            let base = PathBuf::from("/var/tmp");
            #[cfg(not(unix))]
            let base = std::env::temp_dir();
            let path = base.join(format!(
                "herdr-registry-store-{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn source(&self, name: &str, text: &str) -> PathBuf {
            let root = self.0.join(name);
            let path = root.join(PATH);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
            root
        }
        fn journal(&self) -> PathBuf {
            self.0.join("journal/active.json")
        }
        fn store(&self) -> RegistryStore {
            RegistryStore::new(vec![(PATH.into(), AGENT.into())], self.journal()).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn remote_for(active: &RegistrySnapshot, publication: u64, text: &str) -> RegistrySnapshot {
        remote_for_channel(active, publication, text, remote::Channel::Staging)
    }

    fn remote_for_channel(
        active: &RegistrySnapshot,
        publication: u64,
        text: &str,
        channel: remote::Channel,
    ) -> RegistrySnapshot {
        let files = vec![(PATH.to_string(), text.to_string())];
        let commit = "b".repeat(40);
        remote_candidate(
            active,
            remote::RemoteRevision {
                origin: remote::DEFAULT_ORIGIN.into(),
                pointer: remote::ChannelPointer {
                    schema: 1,
                    channel,
                    generation: publication,
                    snapshot_sha256: format!("{publication:064x}"),
                    snapshot_bytes: 100,
                },
                commit: commit.clone(),
            },
            remote::VerifiedSnapshot {
                content_sha256: source_digest(&files),
                files,
                commit,
            },
        )
        .unwrap()
    }

    #[test]
    fn remote_activation_persists_offline_identity_and_offline_reload_keeps_remote_source() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let initial = store.snapshot();
        let text = AGENT.replace("aliases = []", "aliases = ['downloaded']");
        let candidate = remote_for(&initial, 10, &text);
        store.activate_remote(&initial, candidate).unwrap();
        let active = store.snapshot();
        assert!(active.profile_by_normalized_alias("downloaded").is_some());
        assert!(initial.profile_by_normalized_alias("downloaded").is_none());
        assert_eq!(active.remote.as_ref().unwrap().pointer.generation, 10);
        let restarted = RegistryStore::startup(None, fixture.journal());
        assert_eq!(restarted.snapshot().remote, active.remote);
        assert_eq!(restarted.snapshot().digest, active.digest);
        let reloaded = restarted.reload(None).unwrap();
        assert_eq!(reloaded.remote, active.remote);
        assert_eq!(reloaded.generation, active.generation);
        assert_eq!(reloaded.digest, active.digest);
    }

    #[test]
    fn concurrent_local_reload_cannot_be_overwritten_by_remote_download() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let expected = store.snapshot();
        let candidate = remote_for(&expected, 10, AGENT);
        let local = fixture.source("local", AGENT);
        store.reload(Some(&local)).unwrap();
        let before = fs::read(fixture.journal()).unwrap();
        assert!(store
            .activate_remote(&expected, candidate)
            .unwrap_err()
            .contains("local registry"));
        assert_eq!(fs::read(fixture.journal()).unwrap(), before);
        assert_eq!(
            store.status().source,
            Some(fs::canonicalize(&local).unwrap())
        );
        assert!(store.require_managed_source(&store.snapshot()).is_err());
        let reset = store.reset().unwrap();
        assert!(reset.source.is_none());
        assert!(reset.remote.is_none());
        assert!(local.join(PATH).exists());
        assert!(store.require_managed_source(&store.snapshot()).is_ok());
    }

    #[test]
    fn remote_publication_is_monotonic_but_new_sequence_can_restore_old_content() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let initial = store.snapshot();
        store
            .activate_remote(&initial, remote_for(&initial, 10, AGENT))
            .unwrap();
        let active = store.snapshot();
        assert_eq!(
            active.generation, initial.generation,
            "identical content is a runtime no-op"
        );
        let newer = remote_for(
            &active,
            11,
            &AGENT.replace("aliases = []", "aliases = ['next']"),
        );
        let mut stale = newer.remote.clone().unwrap();
        stale.pointer.generation = 9;
        let verified = remote::VerifiedSnapshot {
            content_sha256: newer.digest.clone(),
            files: (*newer.files).clone(),
            commit: stale.commit.clone(),
        };
        assert!(remote_candidate(&active, stale, verified)
            .unwrap_err()
            .contains("older"));
        let mut reused = newer.remote.clone().unwrap();
        reused.pointer.generation = 10;
        let verified = remote::VerifiedSnapshot {
            content_sha256: newer.digest.clone(),
            files: (*newer.files).clone(),
            commit: reused.commit.clone(),
        };
        assert!(remote_candidate(&active, reused, verified)
            .unwrap_err()
            .contains("reused"));
        store.activate_remote(&active, newer).unwrap();
        let current = store.snapshot();
        let rollback = remote_for(&current, 12, AGENT);
        store.activate_remote(&current, rollback).unwrap();
        assert_eq!(store.snapshot().digest, initial.digest);
        assert!(store.snapshot().generation > current.generation);
    }

    #[test]
    fn channel_high_water_marks_survive_switch_restart_and_bundled_reset() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let initial = store.snapshot();
        store
            .activate_remote(
                &initial,
                remote_for_channel(&initial, 10, AGENT, remote::Channel::Stable),
            )
            .unwrap();
        let stable = store.snapshot();
        let accepted = stable.remote.clone().unwrap();
        store
            .activate_remote(
                &stable,
                remote_for_channel(&stable, 1, AGENT, remote::Channel::Preview),
            )
            .unwrap();
        let restarted = RegistryStore::startup(None, fixture.journal());
        assert_eq!(restarted.snapshot().accepted_remote.len(), 2);
        for reset in [false, true] {
            if reset {
                restarted.reset().unwrap();
            }
            let active = restarted.snapshot();
            let before = fs::read(fixture.journal()).unwrap();
            for (generation, hash) in [(9, format!("{:064x}", 9)), (10, "f".repeat(64))] {
                let mut stale = accepted.clone();
                stale.pointer.generation = generation;
                stale.pointer.snapshot_sha256 = hash;
                let files = vec![(PATH.into(), AGENT.into())];
                let candidate = remote_candidate(
                    &active,
                    stale,
                    remote::VerifiedSnapshot {
                        content_sha256: source_digest(&files),
                        files,
                        commit: accepted.commit.clone(),
                    },
                );
                assert!(
                    candidate.is_err(),
                    "stale/reused stable publication must not pass after preview or reset"
                );
                assert_eq!(fs::read(fixture.journal()).unwrap(), before);
                assert!(Arc::ptr_eq(&active, &restarted.snapshot()));
            }
        }
        let active = restarted.snapshot();
        let rollback = remote_for_channel(&active, 11, AGENT, remote::Channel::Stable);
        restarted.activate_remote(&active, rollback).unwrap();
        let recovered = RegistryStore::startup(None, fixture.journal());
        assert_eq!(
            recovered
                .snapshot()
                .remote
                .as_ref()
                .unwrap()
                .pointer
                .generation,
            11
        );
        assert_eq!(recovered.snapshot().accepted_remote.len(), 2);
        let local = fixture.source("explicit-local", AGENT);
        let selected_local = RegistryStore::startup(Some(local), fixture.journal());
        assert_eq!(
            selected_local.snapshot().accepted_remote,
            recovered.snapshot().accepted_remote
        );
        selected_local.reset().unwrap();
        assert_eq!(
            RegistryStore::startup(None, fixture.journal())
                .snapshot()
                .accepted_remote,
            recovered.snapshot().accepted_remote
        );
    }

    #[test]
    fn concurrent_channel_switch_back_cannot_erase_newer_publication_history() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let initial = store.snapshot();
        store
            .activate_remote(
                &initial,
                remote_for_channel(&initial, 10, AGENT, remote::Channel::Stable),
            )
            .unwrap();
        let expected = store.snapshot();
        let delayed = remote_for_channel(&expected, 1, AGENT, remote::Channel::Preview);
        store
            .activate_remote(
                &expected,
                remote_for_channel(&expected, 10, AGENT, remote::Channel::Preview),
            )
            .unwrap();
        let preview = store.snapshot();
        store
            .activate_remote(
                &preview,
                remote_for_channel(&preview, 10, AGENT, remote::Channel::Stable),
            )
            .unwrap();
        let before = store.snapshot();
        assert_eq!(before.generation, expected.generation);
        assert_eq!(before.remote, expected.remote);
        let journal = fs::read(fixture.journal()).unwrap();
        assert!(store
            .activate_remote(&expected, delayed)
            .unwrap_err()
            .contains("changed during download"));
        assert_eq!(fs::read(fixture.journal()).unwrap(), journal);
        assert!(Arc::ptr_eq(&before, &store.snapshot()));
        let restored = RegistryStore::startup(None, fixture.journal());
        assert_eq!(
            restored
                .snapshot()
                .accepted_remote
                .iter()
                .find(|entry| entry.pointer.channel == remote::Channel::Preview)
                .unwrap()
                .pointer
                .generation,
            10
        );
    }

    #[test]
    fn journal_rejects_duplicate_or_unbounded_channel_history() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let active = store.snapshot();
        store
            .activate_remote(&active, remote_for(&active, 1, AGENT))
            .unwrap();
        let mut journal: serde_json::Value =
            serde_json::from_slice(&fs::read(fixture.journal()).unwrap()).unwrap();
        let entry = journal["accepted_remote"][0].clone();
        journal["accepted_remote"] = serde_json::json!([entry.clone(), entry.clone()]);
        assert!(serde_json::from_value::<Journal>(journal.clone()).is_err());
        journal["accepted_remote"] = serde_json::Value::Array(
            (0..=MAX_REMOTE_HISTORY)
                .map(|i| {
                    let mut entry = entry.clone();
                    entry["origin"] = serde_json::json!(format!("https://registry-{i}.example"));
                    entry
                })
                .collect(),
        );
        assert!(serde_json::from_value::<Journal>(journal).is_err());
    }

    #[test]
    fn generation_refresh_during_download_requires_retry_and_failure_keeps_journal() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let initial = store.snapshot();
        let candidate = remote_for(&initial, 10, AGENT);
        store
            .replace_detection(|snapshot| snapshot.manifests.clone())
            .unwrap();
        let journal = fs::read(fixture.journal()).unwrap();
        assert!(store
            .activate_remote(&initial, candidate)
            .unwrap_err()
            .contains("changed during download"));
        assert_eq!(fs::read(fixture.journal()).unwrap(), journal);
    }

    #[test]
    fn remote_persistence_failure_never_publishes_candidate() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let initial = store.snapshot();
        fs::write(fixture.0.join("journal"), "not a directory").unwrap();
        assert!(store
            .activate_remote(&initial, remote_for(&initial, 10, AGENT))
            .is_err());
        assert!(Arc::ptr_eq(&store.snapshot(), &initial));
    }

    #[test]
    fn automatic_and_manual_updates_share_the_nonblocking_download_guard() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let active = store.snapshot();
        let _download = store.download_lock.lock().unwrap();
        for channel in [None, Some(remote::Channel::Stable)] {
            assert!(store
                .update_remote(channel)
                .unwrap_err()
                .contains("already in progress"));
            assert!(Arc::ptr_eq(&active, &store.snapshot()));
            assert!(!fixture.journal().exists());
        }
    }

    #[test]
    fn automatic_updates_follow_the_accepted_channel_and_skip_local_sources() {
        let fixture = Fixture::new();
        let store = fixture.store();
        assert_eq!(
            store.automatic_update_channel(),
            Some(remote::Channel::Stable)
        );
        let active = store.snapshot();
        store.publish(remote_for_channel(
            &active,
            1,
            AGENT,
            remote::Channel::Preview,
        ));
        assert_eq!(
            store.automatic_update_channel(),
            Some(remote::Channel::Preview)
        );
        let source = fixture.source("local", AGENT);
        store.reload(Some(&source)).unwrap();
        let before = store.snapshot();
        let journal = fs::read(fixture.journal()).unwrap();
        assert_eq!(store.automatic_update_channel(), None);
        assert!(Arc::ptr_eq(&before, &store.snapshot()));
        assert_eq!(fs::read(fixture.journal()).unwrap(), journal);
        assert!(store.status().last_error.is_none());

        let selected = RegistryStore::startup(Some(fixture.0.join("missing")), fixture.journal());
        assert!(selected.snapshot().source.is_none());
        assert_eq!(selected.automatic_update_channel(), None);
    }

    #[test]
    fn configured_local_source_never_recovers_an_unrelated_journal() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let selected_a = fixture.source("a", AGENT);
        store.reload(Some(&selected_a)).unwrap();
        let selected_b = fixture.source(
            "b",
            &AGENT.replace("aliases = []", "aliases = ['selected-b']"),
        );
        let restarted = RegistryStore::startup(Some(selected_b.clone()), fixture.journal());
        assert_eq!(
            restarted.status().source,
            Some(fs::canonicalize(selected_b).unwrap())
        );
        assert!(restarted
            .snapshot()
            .profile_by_normalized_alias("selected-b")
            .is_some());
        assert!(restarted
            .status()
            .last_error
            .unwrap()
            .contains("different selected source"));
    }

    #[test]
    fn source_reload_is_complete_persisted_and_old_arc_remains_valid() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let old = store.snapshot();
        assert!(
            !fixture.journal().exists(),
            "initialization must never write"
        );
        let root = fixture.source(
            "source",
            &AGENT.replace("aliases = []", "aliases = ['future-alias']"),
        );
        let status = store.reload(Some(&root)).unwrap();
        assert_eq!(status.generation, 2);
        assert_eq!(store.generation.load(Ordering::Acquire), 2);
        assert_eq!(old.generation, 1);
        assert!(old.profile_by_normalized_alias("future-alias").is_none());
        assert!(store
            .snapshot()
            .profile_by_normalized_alias("future-alias")
            .is_some());
        assert!(store.snapshot().profile_by_id("claude").is_none());
        assert!(!Arc::ptr_eq(&old, &store.snapshot()));
        let persisted = read_journal(&fixture.journal()).unwrap().unwrap();
        assert_eq!(persisted.digest, status.digest);
        assert_eq!(persisted.generation, 2);
        assert_eq!(
            persisted.files[0].content,
            AGENT.replace("aliases = []", "aliases = ['future-alias']")
        );
    }

    #[test]
    fn identical_bytes_keep_generation_but_update_and_retain_provenance() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let first = fixture.source("first", AGENT);
        let second = fixture.source("second", AGENT);
        store.reload(Some(&first)).unwrap();
        let original = store.snapshot();
        let status = store.reload(Some(&second)).unwrap();
        assert_eq!(status.generation, 1);
        assert_eq!(status.source, Some(fs::canonicalize(&second).unwrap()));
        assert!(Arc::ptr_eq(&original.registry, &store.snapshot().registry));
        fs::write(
            second.join(PATH),
            AGENT.replace("Future agent", "New display name"),
        )
        .unwrap();
        assert_eq!(store.reload(None).unwrap().generation, 2);
    }

    #[test]
    fn invalid_read_and_semantic_candidates_preserve_exact_active_arc_and_journal() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let source = fixture.source("source", AGENT);
        store.reload(Some(&source)).unwrap();
        let old = store.snapshot();
        let journal = fs::read(fixture.journal()).unwrap();
        assert!(store.reload(Some(&fixture.0.join("missing"))).is_err());
        assert!(Arc::ptr_eq(&old, &store.snapshot()));
        fs::write(source.join(PATH), "schema = 999").unwrap();
        assert!(store.reload(None).is_err());
        assert!(Arc::ptr_eq(&old, &store.snapshot()));
        assert_eq!(fs::read(fixture.journal()).unwrap(), journal);
        assert!(store.status().last_error.is_some());
        fs::write(source.join(PATH), AGENT).unwrap();
        assert!(store.reload(None).unwrap().last_error.is_none());
    }

    #[test]
    fn invalid_detection_candidate_cannot_publish() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let source = fixture.source("source", AGENT);
        fs::write(source.join("agents/future-agent/detection.toml"),
            "id = 'future-agent'\nversion = '2026.01.01.1'\nmin_engine_version = 1\nupdated_at = '2026-01-01T00:00:00Z'\n[[rules]]\nid = 'idle'\nstate = 'idle'\nregex = ['[']\n").unwrap();
        let old = store.snapshot();
        assert!(store.reload(Some(&source)).is_err());
        assert!(Arc::ptr_eq(&old, &store.snapshot()));
        assert!(!fixture.journal().exists());
    }

    #[test]
    fn journal_write_failure_preserves_active_and_retained_source() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let source = fixture.source("source", &AGENT.replace("Future agent", "Changed"));
        let old = store.snapshot();
        fs::write(fixture.journal().parent().unwrap(), b"not a directory").unwrap();
        assert!(store.reload(Some(&source)).is_err());
        assert!(Arc::ptr_eq(&old, &store.snapshot()));
        assert!(store.configured_source.lock().unwrap().is_none());
        assert_eq!(store.generation.load(Ordering::Acquire), 1);
    }

    #[test]
    fn missing_relative_source_keeps_canonical_parent_identity() {
        let fixture = Fixture::new();
        let name = fixture.0.file_name().unwrap();
        let relative = PathBuf::from(name).join("missing-source");
        let canonical = fs::canonicalize(std::env::current_dir().unwrap()).unwrap();
        assert!(!relative.exists());
        assert_eq!(source_identity(&relative), canonical.join(&relative));
        assert_ne!(
            source_identity(&relative),
            source_identity(&relative.with_file_name("different-source"))
        );
    }

    #[test]
    fn restart_prefers_valid_journal_even_when_source_is_deleted_or_invalid() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let source = fixture.source("source", &AGENT.replace("Future agent", "Saved"));
        let saved = store.reload(Some(&source)).unwrap();
        let bytes = fs::read(fixture.journal()).unwrap();
        fs::remove_dir_all(&source).unwrap();
        assert_eq!(
            source_identity(&source),
            source_identity(saved.source.as_ref().unwrap())
        );
        let restored = RegistryStore::startup(Some(source.clone()), fixture.journal());
        assert_eq!(restored.status().digest, saved.digest);
        assert_eq!(restored.status().generation, saved.generation);
        assert_eq!(
            restored.snapshot().files[0].1,
            AGENT.replace("Future agent", "Saved")
        );
        assert!(restored.snapshot().profile_by_id("claude").is_none());
        assert_eq!(fs::read(fixture.journal()).unwrap(), bytes);
        fixture.source("source", "schema = 999");
        let restored = RegistryStore::startup(Some(source), fixture.journal());
        assert_eq!(restored.status().digest, saved.digest);
        assert_eq!(fs::read(fixture.journal()).unwrap(), bytes);
    }

    #[test]
    fn startup_source_is_complete_and_read_only_without_a_journal() {
        let fixture = Fixture::new();
        let source = fixture.source("source", AGENT);
        let store = RegistryStore::startup(Some(source), fixture.journal());
        assert!(store.snapshot().profile_by_id("future-agent").is_some());
        assert!(store.snapshot().profile_by_id("claude").is_none());
        assert!(!fixture.journal().exists());
    }

    #[test]
    fn invalid_startup_source_falls_back_to_bundled_and_records_error_without_writes() {
        let fixture = Fixture::new();
        let store = RegistryStore::startup(Some(fixture.0.join("missing")), fixture.journal());
        assert!(store.snapshot().profile_by_id("claude").is_some());
        assert!(store.status().source.is_none());
        assert!(store.status().last_error.unwrap().contains("using bundled"));
        assert!(!fixture.journal().exists());
    }

    #[test]
    fn journal_requires_hashes_and_semantics_not_just_valid_json() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let source = fixture.source("source", AGENT);
        store.reload(Some(&source)).unwrap();
        let bytes = fs::read(fixture.journal()).unwrap();
        let mut journal: Journal = serde_json::from_slice(&bytes).unwrap();
        journal.files[0].content = "schema = 999".into();
        fs::write(fixture.journal(), serde_json::to_vec(&journal).unwrap()).unwrap();
        assert!(read_journal(&fixture.journal())
            .unwrap_err()
            .contains("hash"));
        journal.files[0].sha256 = hash(&journal.files[0].content);
        fs::write(fixture.journal(), serde_json::to_vec(&journal).unwrap()).unwrap();
        assert!(read_journal(&fixture.journal())
            .unwrap_err()
            .contains("digest"));
        journal.digest = source_digest(&vec![(PATH.into(), journal.files[0].content.clone())]);
        fs::write(fixture.journal(), serde_json::to_vec(&journal).unwrap()).unwrap();
        assert!(read_journal(&fixture.journal())
            .unwrap()
            .unwrap()
            .into_snapshot()
            .is_err());
    }

    #[test]
    fn journal_selection_requires_valid_matching_local_provenance() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let source = fixture.source("source", AGENT);
        store.reload(Some(&source)).unwrap();
        let bytes = fs::read(fixture.journal()).unwrap();
        for selected in [PathBuf::from("relative"), fixture.0.join("different")] {
            let mut journal: Journal = serde_json::from_slice(&bytes).unwrap();
            journal.selected_source = Some(selected);
            fs::write(fixture.journal(), serde_json::to_vec(&journal).unwrap()).unwrap();
            assert!(read_journal(&fixture.journal()).is_err());
        }
        let mut journal: Journal = serde_json::from_slice(&bytes).unwrap();
        journal.source = None;
        let revision = remote_for(&store.snapshot(), 1, AGENT).remote.unwrap();
        journal.remote = Some(revision.clone());
        journal.accepted_remote = vec![revision];
        fs::write(fixture.journal(), serde_json::to_vec(&journal).unwrap()).unwrap();
        assert!(read_journal(&fixture.journal())
            .unwrap_err()
            .contains("local and remote"));
    }

    #[test]
    fn corrupt_journal_is_not_overwritten_during_fallback() {
        let fixture = Fixture::new();
        fs::create_dir_all(fixture.journal().parent().unwrap()).unwrap();
        fs::write(fixture.journal(), b"corrupt").unwrap();
        let source = fixture.source("source", AGENT);
        let store = RegistryStore::startup(Some(source), fixture.journal());
        assert!(store
            .status()
            .last_error
            .unwrap()
            .contains("last-known-good"));
        assert_eq!(fs::read(fixture.journal()).unwrap(), b"corrupt");
    }

    #[test]
    fn reload_busy_does_not_queue_and_detection_replacement_invalidates_generation() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let old = store.snapshot();
        let guard = store.reload_lock.lock().unwrap();
        assert!(store.reload(None).unwrap_err().contains("busy"));
        assert!(store
            .replace_detection(|s| s.manifests.clone())
            .unwrap_err()
            .contains("busy"));
        assert!(Arc::ptr_eq(&old, &store.snapshot()));
        drop(guard);
        store.replace_detection(|s| s.manifests.clone()).unwrap();
        let current = store.snapshot();
        assert_eq!(current.generation, old.generation + 1);
        assert_eq!(store.generation.load(Ordering::Acquire), current.generation);
        assert_eq!(current.digest, old.digest);
        assert!(Arc::ptr_eq(&current.registry, &old.registry));
        assert_eq!(
            read_journal(&fixture.journal())
                .unwrap()
                .unwrap()
                .generation,
            current.generation
        );
    }

    #[test]
    fn detection_persistence_failure_also_preserves_active_arc() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let old = store.snapshot();
        fs::create_dir_all(fixture.journal()).unwrap();
        assert!(store.replace_detection(|s| s.manifests.clone()).is_err());
        assert!(Arc::ptr_eq(&old, &store.snapshot()));
    }

    #[test]
    fn errors_are_utf8_bounded_and_generation_never_wraps() {
        let error = bounded_error("é".repeat(5000));
        assert!(error.len() <= MAX_ERROR_BYTES);
        assert!(error.ends_with("..."));
        assert!(next_generation(u64::MAX).is_err());
    }

    #[test]
    fn journal_inventory_and_encoded_size_are_bounded() {
        let fixture = Fixture::new();
        fs::create_dir_all(fixture.journal().parent().unwrap()).unwrap();
        let file = fs::File::create(fixture.journal()).unwrap();
        file.set_len(MAX_JOURNAL_BYTES + 1).unwrap();
        assert!(read_journal(&fixture.journal())
            .unwrap_err()
            .contains("bounded"));
        drop(file);
        let record = JournalFile {
            path: PATH.into(),
            content: String::new(),
            sha256: hash(""),
        };
        let record = serde_json::to_string(&record).unwrap();
        let text = format!(
            r#"{{"schema":1,"generation":1,"digest":"","source":null,"files":[{}]}}"#,
            vec![record; files::MAX_FILES + 1].join(",")
        );
        fs::write(fixture.journal(), text).unwrap();
        assert!(read_journal(&fixture.journal())
            .unwrap_err()
            .contains("limits"));
    }

    #[cfg(unix)]
    #[test]
    fn journal_symlinks_and_nonregular_files_are_rejected() {
        let fixture = Fixture::new();
        fs::create_dir_all(fixture.journal().parent().unwrap()).unwrap();
        let target = fixture.0.join("target");
        fs::write(&target, b"untouched").unwrap();
        std::os::unix::fs::symlink(&target, fixture.journal()).unwrap();
        assert!(read_journal(&fixture.journal()).is_err());
        let store = fixture.store();
        assert!(store.replace_detection(|s| s.manifests.clone()).is_err());
        assert_eq!(fs::read(target).unwrap(), b"untouched");
        fs::remove_file(fixture.journal()).unwrap();
        let _socket = std::os::unix::net::UnixListener::bind(fixture.journal()).unwrap();
        assert!(read_journal(&fixture.journal()).is_err());
    }

    fn compiled_integration_source(fixture: &Fixture) -> (RegistryStore, PathBuf) {
        let files: SourceFiles = bundled::FILES
            .iter()
            .filter(|(path, _)| path.starts_with("agents/claude/"))
            .map(|(path, text)| ((*path).into(), (*text).into()))
            .collect();
        let source = fixture.0.join("compiled-source");
        for (path, text) in &files {
            let path = source.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        let store = RegistryStore::new(files, fixture.journal()).unwrap();
        store.reload(Some(&source)).unwrap(); // establish durable LKG first
        (store, source)
    }

    fn save_obsolete_integration_journal(fixture: &Fixture, managed: bool) -> Vec<u8> {
        let mut journal: Journal =
            serde_json::from_slice(&fs::read(fixture.journal()).unwrap()).unwrap();
        let asset = journal
            .files
            .iter_mut()
            .find(|file| file.path == "agents/claude/assets/herdr-agent-state.ps1")
            .unwrap();
        // Model bytes accepted by an older binary but incompatible with this one.
        asset
            .content
            .push_str("\n# previous compiled integration\n");
        asset.sha256 = hash(&asset.content);
        journal.digest = source_digest(
            &journal
                .files
                .iter()
                .map(|file| (file.path.clone(), file.content.clone()))
                .collect(),
        );
        let revision = remote_for(&fixture.store().snapshot(), 20, AGENT)
            .remote
            .unwrap();
        journal.accepted_remote = vec![revision.clone()];
        if managed {
            journal.source = None;
            journal.remote = Some(revision);
        }
        let mut value = serde_json::to_value(journal).unwrap();
        // Exercise migration from the original journal without a separate selection field.
        value.as_object_mut().unwrap().remove("selected_source");
        let bytes = serde_json::to_vec(&value).unwrap();
        fs::write(fixture.journal(), &bytes).unwrap();
        bytes
    }

    #[test]
    fn incompatible_journal_preserves_local_selection_through_fallback_and_refresh() {
        let fixture = Fixture::new();
        let (old, source) = compiled_integration_source(&fixture);
        let selected = old.status().source.unwrap();
        let files = old.snapshot().files.clone();
        let bytes = save_obsolete_integration_journal(&fixture, false);
        fs::remove_dir_all(&source).unwrap();
        let recovered = RegistryStore::startup(None, fixture.journal());
        assert!(recovered
            .status()
            .last_error
            .unwrap()
            .contains("without changing its version"));
        assert!(
            recovered.snapshot().source.is_none(),
            "active fallback is bundled, not local"
        );
        assert_eq!(
            *recovered.configured_source.lock().unwrap(),
            Some(selected.clone())
        );
        assert_eq!(recovered.automatic_update_channel(), None);
        assert_eq!(
            recovered.snapshot().accepted_remote[0].pointer.generation,
            20
        );
        assert_eq!(
            fs::read(fixture.journal()).unwrap(),
            bytes,
            "startup is read-only"
        );

        recovered
            .replace_detection(|snapshot| snapshot.manifests.clone())
            .unwrap();
        let restarted = RegistryStore::startup(None, fixture.journal());
        assert_eq!(*restarted.configured_source.lock().unwrap(), Some(selected));
        assert_eq!(restarted.automatic_update_channel(), None);
        assert_eq!(
            restarted.snapshot().accepted_remote[0].pointer.generation,
            20
        );

        for (path, text) in files.iter() {
            let path = source.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        let repaired = RegistryStore::startup(None, fixture.journal());
        assert_eq!(
            repaired.status().source,
            Some(fs::canonicalize(&source).unwrap())
        );
        assert_eq!(repaired.snapshot().known_profiles().count(), 1);
        repaired.reset().unwrap();
        let reset = RegistryStore::startup(None, fixture.journal());
        assert!(reset.automatic_update_channel().is_some());
        assert_eq!(reset.snapshot().accepted_remote[0].pointer.generation, 20);
    }

    #[test]
    fn incompatible_journal_preserves_remote_history_until_compatible_roll_forward() {
        let fixture = Fixture::new();
        compiled_integration_source(&fixture);
        let bytes = save_obsolete_integration_journal(&fixture, true);
        let recovered = RegistryStore::startup(None, fixture.journal());
        assert!(recovered
            .status()
            .last_error
            .unwrap()
            .contains("without changing its version"));
        assert!(
            recovered.snapshot().remote.is_none(),
            "do not label bundled bytes as remote"
        );
        assert_eq!(recovered.snapshot().accepted_remote.len(), 1);
        assert_eq!(fs::read(fixture.journal()).unwrap(), bytes);
        recovered
            .replace_detection(|snapshot| snapshot.manifests.clone())
            .unwrap();
        let restarted = RegistryStore::startup(None, fixture.journal());
        let active = restarted.snapshot();
        let accepted = active.accepted_remote[0].clone();
        for generation in [19, 20] {
            let mut revision = accepted.clone();
            revision.pointer.generation = generation;
            revision.pointer.snapshot_sha256 = "f".repeat(64);
            let files = vec![(PATH.into(), AGENT.into())];
            assert!(
                remote_candidate(
                    &active,
                    revision,
                    remote::VerifiedSnapshot {
                        content_sha256: source_digest(&files),
                        files,
                        commit: accepted.commit.clone(),
                    }
                )
                .is_err(),
                "reject downgrade and equal-generation replacement after upgrade"
            );
        }
        let next = remote_for(&active, 21, AGENT);
        restarted.activate_remote(&active, next).unwrap();
        assert_eq!(
            RegistryStore::startup(None, fixture.journal())
                .snapshot()
                .remote
                .as_ref()
                .unwrap()
                .pointer
                .generation,
            21
        );
    }

    #[test]
    fn versioned_integration_update_activates_and_survives_restart_without_installing() {
        let fixture = Fixture::new();
        let (store, source) = compiled_integration_source(&fixture);
        let old = store.snapshot();
        let journal = fs::read(fixture.journal()).unwrap();
        let path = source.join("agents/claude/integration.toml");
        let metadata = fs::read_to_string(&path).unwrap();
        let definition: super::super::source::IntegrationDefinition =
            toml::from_str(&metadata).unwrap();
        let previous = definition.versions.unix;
        assert_eq!(previous, definition.versions.windows);
        let next = previous + 1;
        fs::write(
            path,
            metadata
                .replace(&format!("unix = {previous}"), &format!("unix = {next}"))
                .replace(
                    &format!("windows = {previous}"),
                    &format!("windows = {next}"),
                ),
        )
        .unwrap();
        for asset in definition.assets {
            let path = source.join("agents/claude").join(asset.path);
            let text = fs::read_to_string(&path).unwrap();
            fs::write(
                path,
                text.replace(
                    &format!("HERDR_INTEGRATION_VERSION={previous}"),
                    &format!("HERDR_INTEGRATION_VERSION={next}"),
                ),
            )
            .unwrap();
        }
        let files = files::read_source(&source).unwrap();
        let borrowed: Vec<_> = files
            .iter()
            .map(|(p, t)| (p.as_str(), t.as_str()))
            .collect();
        assert!(
            super::super::validate_packages(&borrowed).is_ok(),
            "offline extraction must accept new assets/versions"
        );
        let status = store.reload(None).unwrap();
        assert!(status.generation > old.generation);
        assert_ne!(fs::read(fixture.journal()).unwrap(), journal);
        let restarted = RegistryStore::startup(None, fixture.journal());
        let current = restarted.snapshot();
        let profile = current
            .profile_by_id("claude")
            .unwrap()
            .integration()
            .unwrap();
        assert_eq!(profile.expected_version(), next);
        let asset = profile
            .asset(crate::integration::builtin::claude::HOOK_INSTALL_NAME)
            .unwrap();
        assert_eq!(
            crate::integration::parse_integration_version(asset),
            Some(next)
        );
        assert_eq!(
            old.profile_by_id("claude")
                .unwrap()
                .integration()
                .unwrap()
                .expected_version(),
            previous
        );
        let untouched = fs::read(fixture.journal()).unwrap();
        let asset_path = source.join("agents/claude/assets/herdr-agent-state.ps1");
        fs::write(
            &asset_path,
            format!(
                "{}\n# reused hot version",
                fs::read_to_string(&asset_path).unwrap()
            ),
        )
        .unwrap();
        assert!(store
            .reload(None)
            .unwrap_err()
            .contains("without changing its version"));
        assert_eq!(fs::read(fixture.journal()).unwrap(), untouched);
    }

    #[test]
    fn compiled_integration_asset_change_is_rejected_even_with_unchanged_version() {
        let fixture = Fixture::new();
        let (store, source) = compiled_integration_source(&fixture);
        let old = store.snapshot();
        let journal = fs::read(fixture.journal()).unwrap();
        // Check the non-host platform too: the safety boundary is portable.
        let path = source.join("agents/claude/assets/herdr-agent-state.ps1");
        let mut text = fs::read_to_string(&path).unwrap();
        text.push_str("\n# changed implementation without version bump\n");
        fs::write(path, text).unwrap();
        let files = files::read_source(&source).unwrap();
        let borrowed: Vec<_> = files
            .iter()
            .map(|(p, t)| (p.as_str(), t.as_str()))
            .collect();
        assert!(super::super::validate_packages(&borrowed).is_ok());
        let error = store.reload(None).unwrap_err();
        assert!(
            error.contains("integration asset") && error.contains("without changing its version")
        );
        assert!(Arc::ptr_eq(&old, &store.snapshot()));
        assert_eq!(fs::read(fixture.journal()).unwrap(), journal);
    }

    #[test]
    fn newer_integration_assets_still_require_identity_and_version_markers() {
        let mut packages = super::super::validate_packages(bundled::FILES).unwrap();
        packages.retain(|package| package.identity.id == "pi");
        let definition = packages[0].integration.as_mut().unwrap();
        let previous = definition.versions.unix;
        definition.versions.unix += 1;
        definition.versions.windows += 1;
        for text in packages[0].assets.values_mut() {
            *text = text.replace(
                &format!("HERDR_INTEGRATION_VERSION={previous}"),
                &format!("HERDR_INTEGRATION_VERSION={}", previous + 1),
            );
        }
        assert!(validate_integration_baseline(&packages).is_ok());
        for marker in ["HERDR_INTEGRATION_ID=", "HERDR_INTEGRATION_VERSION="] {
            let mut missing = packages.clone();
            let text = missing[0]
                .assets
                .get_mut("assets/herdr-agent-state.ts")
                .unwrap();
            *text = text
                .lines()
                .filter(|line| !line.contains(marker))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(validate_integration_baseline(&missing)
                .unwrap_err()
                .contains("requires identity and version markers"));
        }
    }

    #[test]
    fn unchanged_compiled_assets_allow_data_driven_integration_labels_and_commands() {
        let fixture = Fixture::new();
        let (store, source) = compiled_integration_source(&fixture);
        let path = source.join("agents/claude/integration.toml");
        let metadata = fs::read_to_string(&path)
            .unwrap()
            .replace("\"claude\"", "\"claude-local\"");
        fs::write(path, metadata).unwrap();
        let status = store.reload(None).unwrap();
        assert_eq!(status.generation, 2);
        let active = store.snapshot();
        let integration = active
            .profile_by_id("claude")
            .unwrap()
            .integration()
            .unwrap();
        assert_eq!(integration.cli_label(), "claude-local");
        assert_eq!(integration.command_names(), ["claude-local"]);
    }

    #[test]
    fn unknown_integration_metadata_remains_valid_but_has_no_compiled_installer() {
        let fixture = Fixture::new();
        let integration = "cli_name = 'future-agent'\naliases = []\n[commands]\nunix = ['future-agent']\nwindows = ['future-agent']\n[supported]\nunix = true\nwindows = true\n[versions]\nunix = 2\nwindows = 2\n[[assets]]\npath = 'assets/reporter.js'\nplatform = 'all'\nrole = 'reporter'\ninstall_name = 'reporter.js'\n";
        let store = RegistryStore::new(
            vec![
                (PATH.into(), AGENT.into()),
                (
                    "agents/future-agent/integration.toml".into(),
                    integration.into(),
                ),
                (
                    "agents/future-agent/assets/reporter.js".into(),
                    "// HERDR_INTEGRATION_ID=future-agent\n// HERDR_INTEGRATION_VERSION=2\n".into(),
                ),
            ],
            fixture.journal(),
        )
        .unwrap();
        assert!(store
            .snapshot()
            .profile_by_id("future-agent")
            .unwrap()
            .integration()
            .is_none());
    }

    #[test]
    fn status_capabilities_describe_only_the_captured_active_registry() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let status = store.status();
        assert_eq!(status.agents.len(), 1);
        let agent = &status.agents[0];
        assert_eq!(agent.id, "future-agent");
        assert!(agent.startable);
        assert!(!agent.process && !agent.detection && !agent.resume && !agent.integration);
        let decoded: RegistryStatus =
            serde_json::from_slice(&serde_json::to_vec(&status).unwrap()).unwrap();
        assert_eq!(decoded.digest, status.digest);
    }

    #[test]
    fn digest_matches_offline_vendor_pin() {
        let files = bundled::FILES
            .iter()
            .map(|(p, t)| ((*p).into(), (*t).into()))
            .collect();
        let lock: serde_json::Value =
            serde_json::from_str(include_str!("../../vendor/agent-registry/lock.json")).unwrap();
        assert_eq!(source_digest(&files), lock["sha256"].as_str().unwrap());
    }
}
