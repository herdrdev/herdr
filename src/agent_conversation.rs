//! Provider-neutral bounded conversation reader.
//!
//! The engine owns durable scanning, provider normalization, live overlays, and
//! cursor state. Public cursors are random registry handles; byte offsets and
//! source metadata remain private to this module.

pub mod claude;
pub mod codex;
pub mod jsonl;
pub mod omp;
pub mod pi;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::Value;

use crate::agent_resume::TranscriptRef;
use crate::api::schema::conversations::{
    ConversationItem, ConversationItemPayload, ConversationPageDirection, ConversationReasonCode,
    ConversationSessionIdentity, ToolStatus,
};

pub const MAX_SCAN_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_ITEMS_PER_PAGE: usize = 256;
pub const MAX_TEXT_BYTES: usize = 8 * 1024;
pub const MAX_DETAIL_BYTES: usize = 16 * 1024;
pub const MAX_PATHS_PER_ITEM: usize = 64;
pub const MAX_PAGE_BYTES: usize = 512 * 1024;
pub const MAX_RECORD_BYTES: usize = 1024 * 1024;
pub const HIGH_SEQUENCE_BASE: u64 = 1 << 40;
const MAX_LOG_ENTRIES: usize = 65_536;
const MAX_CURSOR_ENTRIES: usize = 256;
const MAX_CHANGE_LOG_ENTRIES: usize = 4_096;
const MAX_OVERLAY_RECORDS: usize = 1_024;

#[derive(Debug, Clone, Copy)]
pub struct ReaderLimits {
    pub max_log_entries: usize,
    pub max_topology_entries: usize,
    pub max_scan_bytes: u64,
    pub max_page_bytes: usize,
    pub max_items_per_page: usize,
}

impl ReaderLimits {
    pub const fn production() -> Self {
        Self {
            max_log_entries: MAX_LOG_ENTRIES,
            max_topology_entries: 65_536,
            max_scan_bytes: MAX_SCAN_BYTES,
            max_page_bytes: MAX_PAGE_BYTES,
            max_items_per_page: MAX_ITEMS_PER_PAGE,
        }
    }

    #[cfg(test)]
    pub const fn test_small() -> Self {
        Self {
            max_log_entries: 4,
            max_topology_entries: 4,
            max_scan_bytes: 1024,
            max_page_bytes: 1024,
            max_items_per_page: 4,
        }
    }
}
const MAX_OVERLAY_BYTES: usize = 512 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscriptError {
    Missing,
    Invalid,
    Unreadable,
    TruncatedOrReplaced,
    ScanLimitExceeded,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFingerprint {
    pub identity_token: String,
    pub size: u64,
    pub modified: SystemTime,
    pub canonical_path: std::path::PathBuf,
}

pub fn fingerprint_for(path: &Path) -> Option<SourceFingerprint> {
    let canonical_path = path.canonicalize().ok()?;
    let identity_token = crate::platform::conversation_source_identity(&canonical_path)?;
    let (size, modified) = crate::platform::conversation_source_size_modified(&canonical_path)?;
    Some(SourceFingerprint {
        identity_token,
        size,
        modified,
        canonical_path,
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct NativeRecord {
    /// Canonical block identity. Tool calls/results share this when paired.
    pub native_id: Option<String>,
    /// Provider tree entry identity used only for active-branch selection.
    pub entry_id: Option<String>,
    pub parent_id: Option<String>,
    pub timestamp_ms: Option<u64>,
    pub turn_id: Option<String>,
    pub payload: ConversationItemPayload,
    /// Non-rendered provider tree node retained for parent traversal.
    pub topology_only: bool,
    /// Start of the complete native JSONL record. Private and never serialized.
    pub anchor: u64,
}

impl NativeRecord {
    #[cfg(test)]
    #[allow(dead_code)]
    pub fn tool(id: &str, status: ToolStatus, label: &str) -> Self {
        Self {
            native_id: Some(id.to_string()),
            entry_id: None,
            parent_id: None,
            timestamp_ms: None,
            turn_id: None,
            payload: ConversationItemPayload::ToolActivity {
                action: "tool".into(),
                label: cap_text(label, MAX_TEXT_BYTES),
                status,
                preview: None,
                detail: None,
                duration_ms: None,
                paths: Vec::new(),
            },
            topology_only: false,
            anchor: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct OverlayRecord {
    pub native_id: Option<String>,
    pub entry_id: Option<String>,
    pub timestamp_ms: Option<u64>,
    pub turn_id: Option<String>,
    pub payload: ConversationItemPayload,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CanonicalEntry {
    pub sequence: u64,
    pub id: String,
    pub native_id: Option<String>,
    pub entry_id: Option<String>,
    pub anchor: u64,
    pub timestamp_ms: Option<u64>,
    pub turn_id: Option<String>,
    pub payload: ConversationItemPayload,
    pub overlay: bool,
    updated_revision: u64,
}

fn canonical_record_id(session: &str, native_id: Option<&str>, anchor: u64) -> String {
    let anchor_bytes = anchor.to_le_bytes();
    match native_id {
        Some(native_id) => hash_id(&[session.as_bytes(), native_id.as_bytes()]),
        None => hash_id(&[session.as_bytes(), &anchor_bytes]),
    }
}

fn hash_id(parts: &[&[u8]]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
        hasher.update([0]);
    }
    hasher
        .finalize()
        .iter()
        .take(16)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorState {
    pub generation: String,
    pub session: String,
    pub source_identity: String,
    pub direction: ConversationPageDirection,
    pub sequence: u64,
    pub revision: u64,
    pub durable_anchor: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorError {
    Invalid,
    Evicted,
}

/// Bounded server-side cursor registry. A client sees only a random handle;
/// the registry keeps the canonical position and private durable anchor.
#[derive(Debug, Clone)]
pub struct CursorRegistry {
    entries: HashMap<String, CursorState>,
    order: VecDeque<String>,
    capacity: usize,
}

impl CursorRegistry {
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            capacity: capacity.max(1),
        }
    }

    pub fn issue(&mut self, state: CursorState) -> String {
        let mut handle = random_handle();
        while self.entries.contains_key(&handle) {
            handle = random_handle();
        }
        self.entries.insert(handle.clone(), state);
        self.order.push_back(handle.clone());
        while self.order.len() > self.capacity {
            if let Some(old) = self.order.pop_front() {
                self.entries.remove(&old);
            }
        }
        handle
    }

    pub fn resolve(&self, handle: &str) -> Result<&CursorState, CursorError> {
        if handle.is_empty() || handle.len() > 128 || !handle.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(CursorError::Invalid);
        }
        self.entries.get(handle).ok_or(CursorError::Evicted)
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

fn random_handle() -> String {
    let mut bytes = [0u8; 32];
    if getrandom::fill(&mut bytes).is_err() {
        // This is only a uniqueness fallback. It does not encode any source
        // identity or durable position.
        let now = format!("{:?}-{}", SystemTime::now(), std::process::id());
        return hash_id(&[now.as_bytes()]);
    }
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub struct SourceReaderState {
    pub fingerprint: SourceFingerprint,
    pub tail_offset: u64,
    pub tail_at_boundary: bool,
    pub older_anchor: u64,
    limits: ReaderLimits,
    pub log: VecDeque<CanonicalEntry>,
    pub max_sequence: u64,
    pub min_sequence: u64,
    next_prepend_sequence: u64,
    pub content_revision: u64,
    pub revision_floor: u64,
    pub more_older: bool,
    topology: Vec<NativeRecord>,
    entry_parents: HashMap<String, Option<String>>,
    entry_parent_order: VecDeque<String>,
    entry_turns: HashMap<String, String>,
    canonical_native_ids: HashMap<String, String>,
    message_ordinals: HashMap<String, u64>,
    active_turn_id: Option<String>,
    active_entry_ids: HashSet<String>,
    active_entry_order: VecDeque<String>,
    active_ancestor_ids: HashSet<String>,
    active_branch_complete: bool,
    latest_entry_id: Option<String>,
    branch_reset: bool,
    identity_collapse_reset: bool,
    topology_branching: bool,
    topology_truncated: bool,
    abandoned_entry_ids: HashSet<String>,
    change_log: VecDeque<(u64, u64)>,
    pending_overlays: Vec<OverlayRecord>,
    overlay_bytes: usize,
}

impl SourceReaderState {
    fn new(fingerprint: SourceFingerprint, limits: ReaderLimits) -> Self {
        Self {
            fingerprint,
            tail_offset: 0,
            tail_at_boundary: true,
            older_anchor: 0,
            limits,
            log: VecDeque::new(),
            max_sequence: HIGH_SEQUENCE_BASE - 1,
            min_sequence: HIGH_SEQUENCE_BASE,
            next_prepend_sequence: HIGH_SEQUENCE_BASE - 1,
            content_revision: 0,
            revision_floor: 0,
            more_older: false,
            topology: Vec::new(),
            entry_parents: HashMap::new(),
            entry_parent_order: VecDeque::new(),
            entry_turns: HashMap::new(),
            canonical_native_ids: HashMap::new(),
            message_ordinals: HashMap::new(),
            active_turn_id: None,
            active_entry_ids: HashSet::new(),
            active_entry_order: VecDeque::new(),
            active_ancestor_ids: HashSet::new(),
            active_branch_complete: false,
            latest_entry_id: None,
            branch_reset: false,
            identity_collapse_reset: false,
            topology_branching: false,
            topology_truncated: false,
            abandoned_entry_ids: HashSet::new(),
            change_log: VecDeque::new(),
            pending_overlays: Vec::new(),
            overlay_bytes: 0,
        }
    }

    #[cfg(test)]
    #[allow(dead_code)]
    fn entry_changed_after(&self, sequence: u64, revision: u64) -> bool {
        self.log
            .iter()
            .any(|entry| entry.sequence == sequence && entry.updated_revision > revision)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReaderPage {
    pub items: Vec<ConversationItem>,
    pub revision: u64,
    pub next_cursor: Option<String>,
    pub previous_cursor: Option<String>,
    pub has_older: bool,
}

pub struct ReaderOutcome {
    pub page: Option<ReaderPage>,
    pub reset: bool,
    pub generation: String,
    pub session: ConversationSessionIdentity,
    pub capability_reason: Option<ConversationReasonCode>,
}

pub struct ConversationReader {
    provider: crate::detect::Agent,
    session: String,
    generation: String,
    state: Option<SourceReaderState>,
    cursors: CursorRegistry,
    pending_overlays: Vec<OverlayRecord>,
    display_root: Option<PathBuf>,
    limits: ReaderLimits,
}

impl ConversationReader {
    pub fn new(
        provider: crate::detect::Agent,
        session: String,
        engine_seed: &str,
        cache_generation: u64,
    ) -> Self {
        Self::new_with_limits(
            provider,
            session,
            engine_seed,
            cache_generation,
            ReaderLimits::production(),
        )
    }

    pub fn new_with_limits(
        provider: crate::detect::Agent,
        session: String,
        engine_seed: &str,
        cache_generation: u64,
        limits: ReaderLimits,
    ) -> Self {
        Self {
            provider,
            session,
            generation: format!("{engine_seed}-{cache_generation}"),
            state: None,
            cursors: CursorRegistry::new(MAX_CURSOR_ENTRIES),
            pending_overlays: Vec::new(),
            display_root: None,
            limits,
        }
    }

    pub fn set_display_root(&mut self, root: &Path) {
        let root = root.is_absolute().then(|| root.to_path_buf());
        if self.display_root != root {
            self.display_root = root;
            self.state = None;
            self.cursors.clear();
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session
    }

    pub fn provider_name(&self) -> &'static str {
        self.adapter().provider_name()
    }

    pub fn read_metadata(&mut self, transcript: &TranscriptRef) -> ReaderOutcome {
        let saved_cursors = std::mem::replace(&mut self.cursors, CursorRegistry::new(0));
        let mut outcome = self.read(transcript, None, ConversationPageDirection::Newest, 1);
        if outcome.reset {
            self.cursors.clear();
        } else {
            self.cursors = saved_cursors;
        }
        if let Some(page) = outcome.page.as_mut() {
            page.next_cursor = None;
            page.previous_cursor = None;
        }
        outcome
    }

    pub fn accept_overlay(&mut self, mut record: OverlayRecord) -> bool {
        if record.native_id.is_none() {
            record.native_id = record
                .entry_id
                .clone()
                .or_else(|| record.turn_id.clone().map(|turn| format!("overlay:{turn}")));
            if record.native_id.is_none() {
                return false;
            }
        }
        record.payload = bounded_payload(&record.payload);
        let record_bytes = serde_json::to_vec(&(
            &record.native_id,
            &record.entry_id,
            &record.turn_id,
            &record.payload,
        ))
        .map(|bytes| bytes.len())
        .unwrap_or(MAX_RECORD_BYTES + 1);
        if record_bytes > MAX_RECORD_BYTES {
            return false;
        }
        if let Some(state) = self.state.as_mut() {
            let duplicate = state.log.iter().any(|entry| {
                entry.native_id == record.native_id && entry.payload == record.payload
            });
            return duplicate || upsert_overlay(state, &self.session, record);
        }
        let pending_bytes: usize = self
            .pending_overlays
            .iter()
            .filter_map(|item| {
                serde_json::to_vec(&(
                    item.native_id.as_ref(),
                    item.entry_id.as_ref(),
                    item.turn_id.as_ref(),
                    &item.payload,
                ))
                .ok()
                .map(|bytes| bytes.len())
            })
            .sum();
        if let Some(native_id) = record.native_id.as_deref() {
            if let Some(index) = self
                .pending_overlays
                .iter()
                .position(|item| item.native_id.as_deref() == Some(native_id))
            {
                let old_bytes = serde_json::to_vec(&(
                    &self.pending_overlays[index].native_id,
                    &self.pending_overlays[index].entry_id,
                    &self.pending_overlays[index].turn_id,
                    &self.pending_overlays[index].payload,
                ))
                .map(|bytes| bytes.len())
                .unwrap_or(MAX_OVERLAY_BYTES + 1);
                if self.pending_overlays[index].payload == record.payload {
                    return true;
                }
                if pending_bytes
                    .saturating_sub(old_bytes)
                    .saturating_add(record_bytes)
                    > MAX_OVERLAY_BYTES
                {
                    return false;
                }
                self.pending_overlays[index] = record;
                return true;
            }
        }
        if self.pending_overlays.len() < MAX_OVERLAY_RECORDS
            && pending_bytes.saturating_add(record_bytes) <= MAX_OVERLAY_BYTES
        {
            self.pending_overlays.push(record);
            return true;
        }
        false
    }

    fn adapter(&self) -> &'static dyn ProviderAdapter {
        match self.provider {
            crate::detect::Agent::Pi => &pi::PiAdapter,
            crate::detect::Agent::Omp => &omp::OmpAdapter,
            crate::detect::Agent::Codex => &codex::CodexAdapter,
            crate::detect::Agent::Claude => &claude::ClaudeAdapter,
            _ => &codex::CodexAdapter,
        }
    }

    fn session_identity(&self) -> ConversationSessionIdentity {
        ConversationSessionIdentity {
            id: self.session.clone(),
        }
    }

    pub fn read(
        &mut self,
        transcript: &TranscriptRef,
        cursor: Option<&str>,
        direction: ConversationPageDirection,
        limit: usize,
    ) -> ReaderOutcome {
        let generation = self.generation.clone();
        let session = self.session_identity();
        let limit = limit.clamp(1, self.limits.max_items_per_page);
        if (matches!(direction, ConversationPageDirection::Newest) && cursor.is_some())
            || (!matches!(direction, ConversationPageDirection::Newest) && cursor.is_none())
        {
            return self.reset_outcome(generation, session);
        }

        let parsed = match cursor {
            Some(raw) => match self.cursors.resolve(raw) {
                Ok(state) => {
                    if state.generation != self.generation
                        || state.session != self.session
                        || state.direction != direction
                    {
                        return self.reset_outcome(generation, session);
                    }
                    Some(state.clone())
                }
                Err(_) => return self.reset_outcome(generation, session),
            },
            None => None,
        };

        let adapter = self.adapter();
        let source = match adapter.validate_source(&transcript.path) {
            Ok(source) => source,
            Err(TranscriptError::Missing) => {
                return ReaderOutcome {
                    page: None,
                    reset: false,
                    generation,
                    session,
                    capability_reason: Some(ConversationReasonCode::TranscriptMissing),
                }
            }
            Err(_) => {
                return ReaderOutcome {
                    page: None,
                    reset: false,
                    generation,
                    session,
                    capability_reason: Some(ConversationReasonCode::TranscriptInvalid),
                }
            }
        };
        let source_changed = self.state.as_ref().is_some_and(|state| {
            state.fingerprint.identity_token != source.identity_token
                || source.size < state.tail_offset
                || (source.size <= state.tail_offset
                    && state.fingerprint.modified != source.modified)
        });
        if source_changed {
            self.state = None;
            self.cursors.clear();
            return self.reset_outcome(generation, session);
        }
        if self.state.is_none() {
            self.state = Some(SourceReaderState::new(source.clone(), self.limits));
        }
        if parsed
            .as_ref()
            .is_some_and(|cursor| cursor.source_identity != source.identity_token)
        {
            return self.reset_outcome(generation, session);
        }
        let provider_name = self.provider_name();
        let display_root = self.display_root.clone();
        let pending_overlays = std::mem::take(&mut self.pending_overlays);
        let state = match self.state.as_mut() {
            Some(state) => state,
            None => return self.reset_outcome(generation, session),
        };
        if parsed.as_ref().is_some_and(|cursor| {
            matches!(direction, ConversationPageDirection::Newer)
                && cursor.revision < state.revision_floor
        }) {
            self.state = None;
            self.cursors.clear();
            return self.reset_outcome(generation, session);
        }
        let session_key = self.session.clone();
        for record in pending_overlays {
            let _ = upsert_overlay(state, &session_key, record);
        }
        let source_path = state.fingerprint.canonical_path.clone();
        let scan_result = jsonl::scan_for_page(
            &source_path,
            state,
            adapter,
            direction,
            parsed.as_ref(),
            &session_key,
            display_root.as_deref(),
        );
        if matches!(scan_result, Err(TranscriptError::TruncatedOrReplaced)) {
            return self.reset_outcome(generation, session);
        }
        if scan_result.is_err() {
            return ReaderOutcome {
                page: None,
                reset: false,
                generation,
                session,
                capability_reason: Some(ConversationReasonCode::SourceUnreadable),
            };
        }
        if state.branch_reset {
            if state.identity_collapse_reset {
                // One-shot non-destructive reset: the repaired canonical state
                // is correct and must survive, but a previously visible
                // sequence disappeared, so the client needs reset_required to
                // drop its stale item. Clearing cursors and keeping state lets
                // the rebuilt read merge with the retained entry instead of
                // recreating the provider-ID twin (which would regrow the
                // ghost when a matching live overlay arrives later).
                state.branch_reset = false;
                state.identity_collapse_reset = false;
                self.cursors.clear();
                return ReaderOutcome {
                    page: None,
                    reset: true,
                    generation,
                    session,
                    capability_reason: None,
                };
            }
            self.state = None;
            self.cursors.clear();
            return self.reset_outcome(generation, session);
        }
        reconcile_pending_overlays(state, &session_key);

        let revision = state.content_revision;
        let selected = select_page(state, parsed.as_ref(), direction, limit);
        let (page_items, returned_entries) = bounded_page_entries(
            &selected,
            provider_name,
            direction,
            &self.session,
            &self.limits,
        );
        let first = returned_entries.first().map(|entry| entry.sequence);
        let last = returned_entries.last().map(|entry| entry.sequence);
        let source_identity = state.fingerprint.identity_token.clone();
        let progress_cursor = if returned_entries.is_empty()
            && state.older_anchor > 0
            && matches!(
                direction,
                ConversationPageDirection::Newest | ConversationPageDirection::Older
            ) {
            Some(
                self.cursors.issue(CursorState {
                    generation: generation.clone(),
                    session: self.session.clone(),
                    source_identity: state.fingerprint.identity_token.clone(),
                    direction: ConversationPageDirection::Older,
                    sequence: parsed
                        .as_ref()
                        .map(|cursor| cursor.sequence)
                        .unwrap_or(state.max_sequence.saturating_add(1)),
                    revision,
                    durable_anchor: state.older_anchor,
                }),
            )
        } else {
            None
        };
        let had_progress_cursor = progress_cursor.is_some();
        let previous_cursor = first
            .and_then(|sequence| {
                let needs_older = state.more_older || state.min_sequence < sequence;
                needs_older.then(|| {
                    self.cursors.issue(CursorState {
                        generation: generation.clone(),
                        session: self.session.clone(),
                        source_identity: source_identity.clone(),
                        direction: ConversationPageDirection::Older,
                        sequence,
                        revision,
                        durable_anchor: returned_entries
                            .first()
                            .map(|entry| entry.anchor)
                            .unwrap_or(0),
                    })
                })
            })
            .or(progress_cursor);
        let next_sequence = if matches!(direction, ConversationPageDirection::Newer) {
            parsed
                .as_ref()
                .map(|cursor| cursor.sequence)
                .unwrap_or(0)
                .max(last.unwrap_or(0))
        } else {
            last.unwrap_or(0)
        };
        let returned_revision = returned_entries
            .last()
            .map(|entry| entry.updated_revision)
            .unwrap_or(0);
        let next_revision = if matches!(direction, ConversationPageDirection::Newer) {
            parsed
                .as_ref()
                .map(|cursor| cursor.revision)
                .unwrap_or(0)
                .max(returned_revision)
        } else {
            revision
        };
        let next_cursor = (next_sequence > 0).then(|| {
            self.cursors.issue(CursorState {
                generation: generation.clone(),
                session: self.session.clone(),
                source_identity,
                direction: ConversationPageDirection::Newer,
                revision: next_revision,
                durable_anchor: returned_entries
                    .last()
                    .map(|entry| entry.anchor)
                    .unwrap_or(0),
                sequence: next_sequence,
            })
        });
        let has_older = had_progress_cursor
            || first.is_some_and(|first| state.more_older || state.min_sequence < first);
        // The older path prepends without inline eviction so every in-window
        // row reaches page selection; trim now (front pops only, releasing
        // already-served history) so the log returns to its bounded size.
        trim_log(state);
        ReaderOutcome {
            page: Some(ReaderPage {
                items: page_items,
                revision,
                next_cursor,
                previous_cursor,
                has_older,
            }),
            reset: false,
            generation,
            session,
            capability_reason: None,
        }
    }

    fn reset_outcome(
        &mut self,
        generation: String,
        session: ConversationSessionIdentity,
    ) -> ReaderOutcome {
        self.state = None;
        self.cursors.clear();
        self.pending_overlays.clear();
        ReaderOutcome {
            page: None,
            reset: true,
            generation,
            session,
            capability_reason: None,
        }
    }
}

pub trait ProviderAdapter: Sync {
    fn provider_name(&self) -> &'static str;
    fn validate_source(&self, path: &Path) -> Result<SourceFingerprint, TranscriptError>;
    fn normalize_line(&self, line: &str) -> Vec<NativeRecord>;
    fn normalize_line_for_display(
        &self,
        line: &str,
        _display_root: Option<&Path>,
    ) -> Vec<NativeRecord> {
        self.normalize_line(line)
    }
    fn select_active_branch(&self, records: Vec<NativeRecord>) -> Vec<NativeRecord> {
        records
    }
    fn select_active_branch_from_tip(
        &self,
        records: Vec<NativeRecord>,
        _tip: Option<&str>,
    ) -> Vec<NativeRecord> {
        self.select_active_branch(records)
    }
}

/// Validates a reported transcript source for a provider label using the
/// provider adapter (path rules plus file identity/readability). Used by the
/// capability computation so availability is honest instead of assuming any
/// reference is readable.
pub fn validate_transcript_source(
    agent: &str,
    path: &Path,
) -> Result<SourceFingerprint, TranscriptError> {
    let adapter: &'static dyn ProviderAdapter = match agent {
        "pi" => &pi::PiAdapter,
        "omp" => &omp::OmpAdapter,
        "codex" => &codex::CodexAdapter,
        "claude" => &claude::ClaudeAdapter,
        _ => return Err(TranscriptError::Invalid),
    };
    adapter.validate_source(path)
}

pub fn provider_roots(provider: &str) -> Vec<PathBuf> {
    let home = std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from));
    let Some(home) = home else { return Vec::new() };
    let expanded = |key: &str, fallback: PathBuf| {
        std::env::var_os(key)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .map(|path| expand_tilde(path, &home))
            .unwrap_or(fallback)
    };
    let root = match provider {
        "pi" => expanded("PI_CODING_AGENT_DIR", home.join(".pi").join("agent")),
        "omp" => {
            if let Some(value) = std::env::var_os("PI_CODING_AGENT_DIR").filter(|v| !v.is_empty()) {
                expand_tilde(PathBuf::from(value), &home)
            } else if let Some(value) = std::env::var_os("PI_CONFIG_DIR").filter(|v| !v.is_empty())
            {
                let path = expand_tilde(PathBuf::from(value), &home);
                if path.is_absolute() {
                    path.join("agent")
                } else {
                    home.join(path).join("agent")
                }
            } else {
                home.join(".omp").join("agent")
            }
        }
        "codex" => expanded("CODEX_HOME", home.join(".codex")),
        "claude" => expanded("CLAUDE_CONFIG_DIR", home.join(".claude")),
        _ => return Vec::new(),
    };
    if root.as_os_str().is_empty() {
        Vec::new()
    } else {
        vec![root]
    }
}

fn expand_tilde(path: PathBuf, home: &Path) -> PathBuf {
    let Some(raw) = path.to_str() else {
        return path;
    };
    if raw == "~" {
        return home.to_path_buf();
    }
    raw.strip_prefix("~/")
        .or_else(|| raw.strip_prefix("~\\"))
        .map(|rest| home.join(rest))
        .unwrap_or(path)
}

pub fn validate_under_root(
    path: &Path,
    roots: &[&Path],
) -> Result<SourceFingerprint, TranscriptError> {
    if !path.is_absolute() || path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
        return Err(TranscriptError::Invalid);
    }
    let canonical = path.canonicalize().map_err(|_| TranscriptError::Missing)?;
    let canonical_roots: Vec<_> = roots
        .iter()
        .filter(|root| !root.as_os_str().is_empty() && root.is_absolute())
        .filter_map(|root| root.canonicalize().ok())
        .collect();
    if canonical_roots.is_empty()
        || !canonical_roots
            .iter()
            .any(|root| canonical.starts_with(root))
    {
        return Err(TranscriptError::Invalid);
    }
    let metadata = std::fs::metadata(&canonical).map_err(|_| TranscriptError::Missing)?;
    if !metadata.is_file() {
        return Err(TranscriptError::Invalid);
    }
    fingerprint_for(&canonical).ok_or(TranscriptError::Unreadable)
}

pub fn cap_text(value: &str, cap: usize) -> String {
    if value.len() <= cap {
        return value.to_string();
    }
    let mut end = 0;
    for (index, character) in value.char_indices() {
        let next = index + character.len_utf8();
        if next > cap {
            break;
        }
        end = next;
    }
    value[..end].to_string()
}

fn cap_optional_text(value: Option<&str>, cap: usize) -> Option<String> {
    value
        .filter(|value| !value.is_empty())
        .map(|value| cap_text(value, cap))
}

fn cap_display_text(value: &str, cap: usize) -> String {
    cap_text(&redact_engine_attachment_paths(value), cap)
}

fn cap_optional_display_text(value: Option<&str>, cap: usize) -> Option<String> {
    value
        .filter(|value| !value.is_empty())
        .map(|value| cap_display_text(value, cap))
}

fn redact_engine_attachment_paths(value: &str) -> String {
    let mut redacted = value.to_string();
    while let Some(marker) = redacted.find("herdr-attachments-") {
        let start = redacted[..marker]
            .char_indices()
            .rev()
            .find(|(_, character)| attachment_path_delimiter(*character))
            .map_or(0, |(index, character)| index + character.len_utf8());
        let marker_end = marker + "herdr-attachments-".len();
        let end = redacted[marker_end..]
            .char_indices()
            .find(|(_, character)| attachment_path_delimiter(*character))
            .map_or(redacted.len(), |(index, _)| marker_end + index);
        redacted.replace_range(start..end, "<attachment>");
    }
    redacted
}

fn attachment_path_delimiter(character: char) -> bool {
    character.is_whitespace()
        || matches!(
            character,
            '`' | '"' | '\'' | '[' | ']' | '(' | ')' | ',' | ';'
        )
}

fn split_engine_attachment_trailer(
    text: &str,
) -> (
    String,
    Vec<crate::api::schema::conversations::AttachmentMetadata>,
) {
    const MARKER: &str = "\n\nAttached files:\n";
    let Some((message, trailer)) = text.rsplit_once(MARKER) else {
        return (text.to_string(), Vec::new());
    };
    let attachments = trailer
        .lines()
        .map(parse_engine_attachment_line)
        .collect::<Option<Vec<_>>>();
    match attachments {
        Some(attachments) if !attachments.is_empty() => (message.to_string(), attachments),
        _ => (text.to_string(), Vec::new()),
    }
}

fn parse_engine_attachment_line(
    line: &str,
) -> Option<crate::api::schema::conversations::AttachmentMetadata> {
    let (path, metadata) = line.rsplit_once(" [")?;
    let metadata = metadata.strip_suffix(']')?;
    let components = path
        .split(['/', '\\'])
        .filter(|component| !component.is_empty())
        .collect::<Vec<_>>();
    let handle = components.last()?;
    if handle.len() != 32
        || !handle.bytes().all(|byte| byte.is_ascii_hexdigit())
        || !components
            .iter()
            .any(|component| component.starts_with("herdr-attachments-"))
    {
        return None;
    }

    let (without_size, byte_size) = metadata
        .rsplit_once("; ")
        .and_then(|(prefix, value)| value.parse::<u64>().ok().map(|size| (prefix, size)))
        .unwrap_or((metadata, 0));
    let (name, media_type) = without_size.rsplit_once("; ")?;
    if name.is_empty()
        || name.len() > 256
        || name.contains(['/', '\\', '\r', '\n', '\0'])
        || !media_type.starts_with("image/")
        || media_type.len() > 128
    {
        return None;
    }
    Some(crate::api::schema::conversations::AttachmentMetadata {
        media_type: media_type.to_string(),
        name: name.to_string(),
        byte_size,
    })
}

pub fn safe_display_path(path: &str) -> Option<String> {
    if path.is_empty()
        || path.len() > MAX_TEXT_BYTES
        || path.contains('\0')
        || path.starts_with('/')
        || path.starts_with('\\')
        || path.as_bytes().get(1) == Some(&b':')
        || path
            .split(['/', '\\'])
            .any(|component| component == ".." || component.is_empty())
    {
        return None;
    }
    Some(path.to_string())
}

pub fn safe_display_path_under_root(path: &str, root: Option<&Path>) -> Option<String> {
    safe_display_path(path).or_else(|| {
        let relative = Path::new(path).strip_prefix(root?).ok()?;
        safe_display_path(relative.to_str()?)
    })
}

pub fn tool_command_preview(action: &str, input: Option<&Value>) -> Option<String> {
    let action = action.to_ascii_lowercase();
    if !(action.contains("bash")
        || action.contains("shell")
        || matches!(
            action.as_str(),
            "exec_command" | "execute_command" | "run_command"
        ))
    {
        return None;
    }
    let input = input?;
    let parsed;
    let input = if let Some(value) = input.as_str() {
        parsed = serde_json::from_str(value).unwrap_or_else(|_| Value::String(value.to_string()));
        &parsed
    } else {
        input
    };
    let command = input
        .get("command")
        .or_else(|| input.get("cmd"))
        .and_then(Value::as_str)
        .or_else(|| input.as_str())?;
    (!command.is_empty()).then(|| cap_text(command, MAX_DETAIL_BYTES))
}

pub fn cap_paths(paths: Vec<String>) -> Vec<String> {
    paths
        .into_iter()
        .filter_map(|path| safe_display_path(&path))
        .take(MAX_PATHS_PER_ITEM)
        .map(|path| cap_text(&path, MAX_TEXT_BYTES))
        .collect()
}

fn bounded_payload(payload: &ConversationItemPayload) -> ConversationItemPayload {
    match payload {
        ConversationItemPayload::UserMessage { text, attachments } => {
            let (text, injected_attachments) = split_engine_attachment_trailer(text);
            ConversationItemPayload::UserMessage {
                text: cap_display_text(&text, MAX_TEXT_BYTES),
                attachments: attachments
                    .iter()
                    .map(
                        |attachment| crate::api::schema::conversations::AttachmentMetadata {
                            media_type: cap_display_text(&attachment.media_type, 128),
                            name: cap_display_text(&attachment.name, 256),
                            byte_size: attachment.byte_size,
                        },
                    )
                    .chain(injected_attachments)
                    .take(16)
                    .collect(),
            }
        }
        ConversationItemPayload::AssistantMessage { phase, text, state } => {
            ConversationItemPayload::AssistantMessage {
                phase: *phase,
                text: cap_display_text(text, MAX_TEXT_BYTES),
                state: *state,
            }
        }
        ConversationItemPayload::PlanUpdate { steps } => ConversationItemPayload::PlanUpdate {
            steps: steps
                .iter()
                .take(64)
                .map(|step| crate::api::schema::conversations::PlanStep {
                    label: cap_display_text(&step.label, 1024),
                    status: step.status,
                })
                .collect(),
        },
        ConversationItemPayload::ToolActivity {
            action,
            label,
            status,
            preview,
            detail,
            duration_ms,
            paths,
        } => ConversationItemPayload::ToolActivity {
            action: cap_text(action, MAX_TEXT_BYTES),
            label: cap_text(label, MAX_TEXT_BYTES),
            status: *status,
            preview: cap_optional_display_text(preview.as_deref(), MAX_DETAIL_BYTES),
            detail: cap_optional_display_text(detail.as_deref(), MAX_DETAIL_BYTES),
            duration_ms: *duration_ms,
            paths: cap_paths(paths.clone()),
        },
        ConversationItemPayload::FileChange {
            path,
            change,
            summary,
        } => ConversationItemPayload::FileChange {
            path: safe_display_path(path).unwrap_or_else(|| "file".into()),
            change: *change,
            summary: cap_optional_display_text(summary.as_deref(), MAX_DETAIL_BYTES),
        },
        ConversationItemPayload::Approval {
            request_id,
            prompt,
            decisions,
            status,
            selected_decision,
            structured_response,
        } => ConversationItemPayload::Approval {
            request_id: cap_text(request_id, MAX_TEXT_BYTES),
            prompt: cap_display_text(prompt, MAX_TEXT_BYTES),
            decisions: decisions
                .iter()
                .take(16)
                .map(
                    |decision| crate::api::schema::conversations::ApprovalDecision {
                        id: cap_text(&decision.id, MAX_TEXT_BYTES),
                        label: cap_display_text(&decision.label, MAX_TEXT_BYTES),
                    },
                )
                .collect(),
            status: *status,
            selected_decision: cap_optional_text(selected_decision.as_deref(), MAX_TEXT_BYTES),
            structured_response: *structured_response,
        },
        ConversationItemPayload::TurnState {
            state,
            started_ms,
            duration_ms,
            error,
        } => ConversationItemPayload::TurnState {
            state: *state,
            started_ms: *started_ms,
            duration_ms: *duration_ms,
            error: cap_optional_display_text(error.as_deref(), MAX_TEXT_BYTES),
        },
        ConversationItemPayload::Notice { message } => ConversationItemPayload::Notice {
            message: cap_display_text(message, MAX_TEXT_BYTES),
        },
    }
}

fn merge_payload(
    existing: &ConversationItemPayload,
    incoming: &ConversationItemPayload,
) -> ConversationItemPayload {
    if let (
        ConversationItemPayload::ToolActivity {
            action: old_action,
            label: old_label,
            preview: old_preview,
            detail: old_detail,
            duration_ms: old_duration_ms,
            paths: old_paths,
            ..
        },
        ConversationItemPayload::ToolActivity {
            action,
            label,
            status,
            preview,
            detail,
            duration_ms,
            paths,
        },
    ) = (existing, incoming)
    {
        return ConversationItemPayload::ToolActivity {
            action: if action == "tool" {
                old_action.clone()
            } else {
                action.clone()
            },
            label: if label == "completed" || label == "failed" {
                label.clone()
            } else {
                old_label.clone()
            },
            status: *status,
            preview: preview.clone().or_else(|| old_preview.clone()),
            detail: detail.clone().or_else(|| old_detail.clone()),
            duration_ms: duration_ms.or(*old_duration_ms),
            paths: old_paths
                .iter()
                .chain(paths.iter())
                .take(MAX_PATHS_PER_ITEM)
                .cloned()
                .collect(),
        };
    }
    bounded_payload(incoming)
}

fn merge_overlay_metadata_into_durable(
    durable: &ConversationItemPayload,
    overlay: &ConversationItemPayload,
) -> ConversationItemPayload {
    let (
        ConversationItemPayload::ToolActivity {
            action,
            label,
            status,
            preview,
            detail,
            duration_ms,
            paths,
        },
        ConversationItemPayload::ToolActivity {
            action: overlay_action,
            preview: overlay_preview,
            detail: overlay_detail,
            duration_ms: overlay_duration_ms,
            paths: overlay_paths,
            ..
        },
    ) = (durable, overlay)
    else {
        return durable.clone();
    };

    let mut merged_paths = paths.clone();
    for path in overlay_paths {
        if merged_paths.len() >= MAX_PATHS_PER_ITEM {
            break;
        }
        if !merged_paths.contains(path) {
            merged_paths.push(path.clone());
        }
    }
    ConversationItemPayload::ToolActivity {
        action: if action == "tool" {
            overlay_action.clone()
        } else {
            action.clone()
        },
        label: label.clone(),
        status: *status,
        preview: preview.clone().or_else(|| overlay_preview.clone()),
        detail: detail.clone().or_else(|| overlay_detail.clone()),
        duration_ms: duration_ms.or(*overlay_duration_ms),
        paths: merged_paths,
    }
}

pub fn entry_to_item(entry: &CanonicalEntry, provider: &str, session: &str) -> ConversationItem {
    ConversationItem {
        id: entry.id.clone(),
        sequence: entry.sequence,
        provider: provider.to_string(),
        session_id: Some(session.to_string()),
        turn_id: entry.turn_id.clone(),
        timestamp_ms: entry.timestamp_ms,
        payload: bounded_payload(&entry.payload),
    }
}

pub fn append_durable(
    state: &mut SourceReaderState,
    session: &str,
    record: &NativeRecord,
    offset: u64,
) {
    append_durable_at(state, session, record, offset, None);
}

fn append_durable_at(
    state: &mut SourceReaderState,
    session: &str,
    record: &NativeRecord,
    offset: u64,
    sequence: Option<u64>,
) {
    let id = canonical_record_id(session, record.native_id.as_deref(), offset);
    if let Some(index) = state.log.iter().position(|entry| {
        entry.id == id || (record.native_id.is_some() && entry.native_id == record.native_id)
    }) {
        let revision = state.content_revision.saturating_add(1);
        let mut changed_sequence = None;
        let mut completed_paths = Vec::new();
        let mut tool_native_id = None;
        if let Some(entry) = state.log.get_mut(index) {
            let payload = merge_payload(&entry.payload, &record.payload);
            if entry.payload != payload
                || entry.timestamp_ms != record.timestamp_ms.or(entry.timestamp_ms)
                || entry.turn_id != record.turn_id.as_ref().or(entry.turn_id.as_ref()).cloned()
                || entry.overlay
            {
                if matches!(
                    &payload,
                    ConversationItemPayload::ToolActivity {
                        status: ToolStatus::Completed,
                        ..
                    }
                ) {
                    if let ConversationItemPayload::ToolActivity { paths, .. } = &payload {
                        completed_paths = paths.clone();
                        tool_native_id = entry.native_id.clone();
                    }
                }
                entry.payload = payload;
                entry.timestamp_ms = record.timestamp_ms.or(entry.timestamp_ms);
                if entry.turn_id.is_none() {
                    entry.turn_id = record.turn_id.clone();
                }
                entry.entry_id = record.entry_id.clone().or(entry.entry_id.clone());
                entry.anchor = offset;
                entry.overlay = false;
                entry.updated_revision = revision;
                changed_sequence = Some(entry.sequence);
            }
        }
        if let Some(sequence) = changed_sequence {
            state.content_revision = revision;
            remember_change(state, sequence);
            for (index, path) in completed_paths.into_iter().enumerate() {
                let file_id = tool_native_id
                    .as_deref()
                    .map(|id| format!("{id}:file:{path}"));
                let file_record = NativeRecord {
                    native_id: file_id,
                    entry_id: record.entry_id.clone(),
                    parent_id: record.parent_id.clone(),
                    timestamp_ms: record.timestamp_ms,
                    turn_id: record.turn_id.clone(),
                    payload: ConversationItemPayload::FileChange {
                        path,
                        change: crate::api::schema::conversations::FileChangeKind::Modified,
                        summary: None,
                    },
                    topology_only: false,
                    anchor: offset.saturating_add(index as u64 + 1),
                };
                append_durable_at(state, session, &file_record, file_record.anchor, None);
            }
        }
        return;
    }
    refresh_overlay_bytes(state);
    let next = sequence.unwrap_or_else(|| state.max_sequence.saturating_add(1));
    let revision = state.content_revision.saturating_add(1);
    state.log.push_back(CanonicalEntry {
        sequence: next,
        id,
        native_id: record.native_id.clone(),
        anchor: offset,
        timestamp_ms: record.timestamp_ms,
        turn_id: record.turn_id.clone(),
        entry_id: record.entry_id.clone(),
        payload: bounded_payload(&record.payload),
        overlay: false,
        updated_revision: revision,
    });
    state.max_sequence = state.max_sequence.max(next);
    state.min_sequence = state.min_sequence.min(next);
    state.content_revision = revision;
    refresh_overlay_bytes(state);
    remember_change(state, next);
    trim_log(state);
}

fn prepend_durable(
    state: &mut SourceReaderState,
    session: &str,
    record: &NativeRecord,
    offset: u64,
) {
    let id = canonical_record_id(session, record.native_id.as_deref(), offset);
    if let Some(index) = state.log.iter().position(|entry| {
        entry.id == id || (record.native_id.is_some() && entry.native_id == record.native_id)
    }) {
        let revision = state.content_revision.saturating_add(1);
        let sequence = state.log[index].sequence;
        let mut changed = false;
        if let Some(entry) = state.log.get_mut(index) {
            let payload = merge_payload(&record.payload, &entry.payload);
            if entry.payload != payload
                || entry.timestamp_ms != record.timestamp_ms.or(entry.timestamp_ms)
                || (entry.turn_id.is_none() && record.turn_id.is_some())
            {
                entry.payload = payload;
                entry.timestamp_ms = record.timestamp_ms.or(entry.timestamp_ms);
                if entry.turn_id.is_none() {
                    entry.turn_id = record.turn_id.clone();
                }
                entry.entry_id = record.entry_id.clone().or(entry.entry_id.clone());
                entry.anchor = offset;
                entry.overlay = false;
                entry.updated_revision = revision;
                changed = true;
            }
        }
        if changed {
            state.content_revision = revision;
            remember_change(state, sequence);
        }
        return;
    }
    // Do not evict inline: entries prepended for the current older page have
    // not been returned yet, and popping the newest (already-returned) entries
    // can evict never-returned in-window rows. The caller trims the log after
    // the page is served (front pops only release already-served history).
    let sequence = state.next_prepend_sequence;
    state.next_prepend_sequence = state.next_prepend_sequence.saturating_sub(1);
    let revision = state.content_revision.saturating_add(1);
    state.log.push_front(CanonicalEntry {
        sequence,
        id,
        native_id: record.native_id.clone(),
        anchor: offset,
        timestamp_ms: record.timestamp_ms,
        turn_id: record.turn_id.clone(),
        entry_id: record.entry_id.clone(),
        payload: bounded_payload(&record.payload),
        overlay: false,
        updated_revision: revision,
    });
    state.min_sequence = sequence;
    state.max_sequence = state
        .log
        .back()
        .map(|entry| entry.sequence)
        .unwrap_or(sequence);
    state.content_revision = revision;
    remember_change(state, sequence);
}

#[cfg(test)]
pub fn append_overlay(state: &mut SourceReaderState, session: &str, record: &OverlayRecord) {
    let _ = upsert_overlay(state, session, record.clone());
}

fn upsert_overlay(state: &mut SourceReaderState, session: &str, record: OverlayRecord) -> bool {
    let native_id = record.native_id.clone();
    let id = canonical_record_id(session, native_id.as_deref(), 0);
    let payload = bounded_payload(&record.payload);
    let payload_bytes = serde_json::to_vec(&payload)
        .map(|bytes| bytes.len())
        .unwrap_or(MAX_OVERLAY_BYTES + 1);
    if payload_bytes > MAX_OVERLAY_BYTES {
        return false;
    }
    if let Some(index) = state
        .log
        .iter()
        .position(|entry| entry.id == id || (native_id.is_some() && entry.native_id == native_id))
    {
        let old_bytes = state
            .log
            .get(index)
            .and_then(|entry| serde_json::to_vec(&entry.payload).ok())
            .map(|bytes| bytes.len())
            .unwrap_or(0);
        if state.log.get(index).is_some_and(|entry| !entry.overlay) {
            let revision = state.content_revision.saturating_add(1);
            let (sequence, completed_paths, anchor, entry_id, timestamp_ms, turn_id) = {
                let Some(entry) = state.log.get_mut(index) else {
                    return false;
                };
                let merged = merge_overlay_metadata_into_durable(&entry.payload, &payload);
                if merged == entry.payload {
                    return false;
                }
                let completed_paths = if let ConversationItemPayload::ToolActivity {
                    status: ToolStatus::Completed,
                    paths,
                    ..
                } = &merged
                {
                    paths.clone()
                } else {
                    Vec::new()
                };
                entry.payload = merged;
                entry.updated_revision = revision;
                (
                    entry.sequence,
                    completed_paths,
                    entry.anchor,
                    entry.entry_id.clone(),
                    entry.timestamp_ms,
                    entry.turn_id.clone(),
                )
            };
            state.content_revision = revision;
            remember_change(state, sequence);
            for (path_index, path) in completed_paths.into_iter().enumerate() {
                let file_record = NativeRecord {
                    native_id: native_id.as_deref().map(|id| format!("{id}:file:{path}")),
                    entry_id: entry_id.clone(),
                    parent_id: None,
                    timestamp_ms,
                    turn_id: turn_id.clone(),
                    payload: ConversationItemPayload::FileChange {
                        path,
                        change: crate::api::schema::conversations::FileChangeKind::Modified,
                        summary: None,
                    },
                    topology_only: false,
                    anchor: anchor.saturating_add(path_index as u64 + 1),
                };
                append_durable_at(state, session, &file_record, file_record.anchor, None);
            }
            return true;
        }
        if state
            .log
            .get(index)
            .is_none_or(|entry| entry.payload == payload)
        {
            return false;
        }
        if state
            .overlay_bytes
            .saturating_sub(old_bytes)
            .saturating_add(payload_bytes)
            > MAX_OVERLAY_BYTES
        {
            return false;
        }
        let revision = state.content_revision.saturating_add(1);
        let sequence = if let Some(entry) = state.log.get_mut(index) {
            entry.payload = payload;
            entry.timestamp_ms = record.timestamp_ms.or(entry.timestamp_ms);
            entry.turn_id = record.turn_id.or(entry.turn_id.clone());
            entry.overlay = true;
            entry.updated_revision = revision;
            entry.sequence
        } else {
            return false;
        };
        state.content_revision = revision;
        refresh_overlay_bytes(state);
        remember_change(state, sequence);
        return true;
    }
    if state.overlay_bytes.saturating_add(payload_bytes) > MAX_OVERLAY_BYTES {
        return false;
    }
    let sequence = state.max_sequence.saturating_add(1);
    let revision = state.content_revision.saturating_add(1);
    state.log.push_back(CanonicalEntry {
        sequence,
        id,
        native_id,
        entry_id: record.entry_id.clone(),
        anchor: 0,
        timestamp_ms: record.timestamp_ms,
        turn_id: record.turn_id,
        payload,
        overlay: true,
        updated_revision: revision,
    });
    state.max_sequence = sequence;
    state.min_sequence = state.min_sequence.min(sequence);
    state.content_revision = revision;
    refresh_overlay_bytes(state);
    remember_change(state, sequence);
    trim_log(state);
    true
}

fn reconcile_pending_overlays(state: &mut SourceReaderState, session: &str) {
    let pending = std::mem::take(&mut state.pending_overlays);
    for record in pending {
        let _ = upsert_overlay(state, session, record);
    }
}

fn refresh_overlay_bytes(state: &mut SourceReaderState) {
    state.overlay_bytes = state
        .log
        .iter()
        .filter(|entry| entry.overlay)
        .filter_map(|entry| {
            serde_json::to_vec(&entry.payload)
                .ok()
                .map(|bytes| bytes.len())
        })
        .sum();
}

fn trim_log(state: &mut SourceReaderState) {
    let mut evicted = false;
    while state.log.len() > state.limits.max_log_entries {
        state.log.pop_front();
        evicted = true;
    }
    if evicted {
        state.more_older = true;
        state.revision_floor = state.content_revision;
    }
    if let Some(first) = state.log.front() {
        state.min_sequence = first.sequence;
    }
}

fn remember_change(state: &mut SourceReaderState, sequence: u64) {
    state
        .change_log
        .retain(|(_, changed_sequence)| *changed_sequence != sequence);
    state
        .change_log
        .push_back((state.content_revision, sequence));
    while state.change_log.len() > MAX_CHANGE_LOG_ENTRIES {
        state.change_log.pop_front();
    }
    let retained_change_floor = state
        .change_log
        .front()
        .map(|(revision, _)| *revision)
        .unwrap_or(state.content_revision);
    state.revision_floor = state.revision_floor.max(retained_change_floor);
}

fn select_page<'a>(
    state: &'a SourceReaderState,
    cursor: Option<&CursorState>,
    direction: ConversationPageDirection,
    limit: usize,
) -> Vec<&'a CanonicalEntry> {
    match direction {
        ConversationPageDirection::Newest => state
            .log
            .iter()
            .rev()
            .take(limit)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect(),
        ConversationPageDirection::Older => {
            let boundary = cursor.map(|cursor| cursor.sequence).unwrap_or(u64::MAX);
            state
                .log
                .iter()
                .filter(|entry| entry.sequence < boundary)
                .rev()
                .take(limit)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect()
        }
        ConversationPageDirection::Newer => {
            let cursor = cursor.expect("validated before page selection");
            let mut changes = state
                .change_log
                .iter()
                .filter(|(revision, sequence)| {
                    *sequence > cursor.sequence || *revision > cursor.revision
                })
                .collect::<Vec<_>>();
            // The change log is normally appended in revision order, but a
            // bounded reader can rebuild/merge retained state from multiple
            // scan directions. Sort before applying the page limit so a
            // newer revision cannot advance the cursor past an older change
            // that was still waiting in the same delta.
            changes.sort_unstable_by_key(|(revision, _)| *revision);
            changes
                .into_iter()
                .take(limit)
                .filter_map(|(_, sequence)| {
                    state.log.iter().find(|entry| entry.sequence == *sequence)
                })
                .collect()
        }
    }
}

fn bounded_page_entries<'a>(
    selected: &'a [&'a CanonicalEntry],
    provider: &str,
    direction: ConversationPageDirection,
    session: &str,
    limits: &ReaderLimits,
) -> (Vec<ConversationItem>, Vec<&'a CanonicalEntry>) {
    let reverse = matches!(
        direction,
        ConversationPageDirection::Newest | ConversationPageDirection::Older
    );
    let mut items = Vec::new();
    let mut entries = Vec::new();
    let candidates: Box<dyn Iterator<Item = &'a CanonicalEntry> + 'a> = if reverse {
        Box::new(selected.iter().rev().copied())
    } else {
        Box::new(selected.iter().copied())
    };
    let mut bytes = 0usize;
    for entry in candidates {
        let item = entry_to_item(entry, provider, session);
        let item_bytes = serde_json::to_vec(&item)
            .map(|value| value.len())
            .unwrap_or(limits.max_page_bytes + 1);
        if !entries.is_empty() && bytes.saturating_add(item_bytes) > limits.max_page_bytes {
            break;
        }
        bytes = bytes.saturating_add(item_bytes);
        entries.push(entry);
        items.push(item);
        if entries.len() >= limits.max_items_per_page {
            break;
        }
    }
    if reverse {
        entries.reverse();
        items.reverse();
    }
    (items, entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_paths_may_be_made_relative_to_an_explicit_root() {
        let root = std::path::Path::new("/workspace/project");

        assert_eq!(
            safe_display_path_under_root("/workspace/project/src/chat.ts", Some(root)),
            Some("src/chat.ts".into())
        );
        assert_eq!(
            safe_display_path_under_root("/workspace/other/secrets.txt", Some(root)),
            None
        );
        assert_eq!(
            safe_display_path_under_root("src/chat.ts", Some(root)),
            Some("src/chat.ts".into())
        );
    }

    #[test]
    fn empty_optional_tool_detail_is_omitted_from_public_items() {
        let payload = bounded_payload(&ConversationItemPayload::ToolActivity {
            action: "bash".into(),
            label: "completed".into(),
            status: ToolStatus::Completed,
            preview: None,
            detail: Some(String::new()),
            duration_ms: None,
            paths: Vec::new(),
        });

        assert!(matches!(
            payload,
            ConversationItemPayload::ToolActivity { detail: None, .. }
        ));
    }

    #[test]
    fn engine_attachment_trailers_become_metadata_and_host_paths_are_redacted() {
        let handle = "a".repeat(32);
        let host_path = format!("/tmp/herdr-attachments-12-1/{handle}");
        let payload = bounded_payload(&ConversationItemPayload::UserMessage {
            text: format!(
                "Inspect this\n\nAttached files:\n{host_path} [sample.png; image/png; 68]"
            ),
            attachments: Vec::new(),
        });
        assert!(matches!(
            payload,
            ConversationItemPayload::UserMessage { text, attachments }
                if text == "Inspect this"
                    && attachments == vec![crate::api::schema::conversations::AttachmentMetadata {
                        media_type: "image/png".into(),
                        name: "sample.png".into(),
                        byte_size: 68,
                    }]
        ));

        let payload = bounded_payload(&ConversationItemPayload::AssistantMessage {
            phase: crate::api::schema::conversations::AssistantMessagePhase::Final,
            text: format!("I inspected `{host_path}`."),
            state: crate::api::schema::conversations::CompletionState::Completed,
        });
        assert!(matches!(
            payload,
            ConversationItemPayload::AssistantMessage { text, .. }
                if text == "I inspected `<attachment>`."
        ));
    }

    pub(crate) fn test_state() -> SourceReaderState {
        SourceReaderState::new(
            SourceFingerprint {
                identity_token: "source".into(),
                size: 0,
                modified: SystemTime::UNIX_EPOCH,
                canonical_path: PathBuf::from("/tmp/source.jsonl"),
            },
            ReaderLimits::production(),
        )
    }

    #[test]
    fn verified_provider_fixtures_normalize_visible_events_only() {
        let cases: [(&str, &dyn ProviderAdapter); 4] = [
            (
                include_str!("agent_conversation/fixtures/pi.jsonl"),
                &pi::PiAdapter,
            ),
            (
                include_str!("agent_conversation/fixtures/omp.jsonl"),
                &omp::OmpAdapter,
            ),
            (
                include_str!("agent_conversation/fixtures/codex.jsonl"),
                &codex::CodexAdapter,
            ),
            (
                include_str!("agent_conversation/fixtures/claude.jsonl"),
                &claude::ClaudeAdapter,
            ),
        ];
        for (fixture, adapter) in cases {
            let records: Vec<_> = fixture
                .lines()
                .flat_map(|line| adapter.normalize_line(line))
                .collect();
            assert!(!records.is_empty());
            assert!(records
                .iter()
                .all(|record| !format!("{:?}", record.payload).contains("private reasoning")));
            assert!(records.iter().any(|record| matches!(
                record.payload,
                ConversationItemPayload::UserMessage { .. }
            )));
            assert!(records.iter().any(|record| matches!(
                record.payload,
                ConversationItemPayload::AssistantMessage { .. }
            )));
            if adapter.provider_name() == "pi" || adapter.provider_name() == "omp" {
                assert!(records.iter().any(|record| matches!(
                    record.payload,
                    ConversationItemPayload::ToolActivity { .. }
                )));
            }
        }
    }

    #[test]
    fn invalid_roots_are_rejected() {
        let path = Path::new("/tmp/outside.jsonl");
        assert!(validate_under_root(path, &[]).is_err());
        assert!(provider_roots("unknown").is_empty());
    }

    #[cfg(unix)]
    pub(crate) fn with_pi_fixture<F: FnOnce(&std::path::Path)>(body: F) {
        let _guard = crate::integration::integration_env_lock();
        let base = std::env::temp_dir().join(format!("herdr-reader-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let old = std::env::var_os("PI_CODING_AGENT_DIR");
        std::env::set_var("PI_CODING_AGENT_DIR", &base);
        body(&base.join("session.jsonl"));
        if let Some(value) = old {
            std::env::set_var("PI_CODING_AGENT_DIR", value);
        } else {
            std::env::remove_var("PI_CODING_AGENT_DIR");
        }
        let _ = std::fs::remove_dir_all(base);
    }

    #[cfg(unix)]
    #[test]
    fn reader_pages_append_boundary_and_stable_ids_across_limits() {
        with_pi_fixture(|path| {
            let mut rows = Vec::new();
            let mut parent = "null".to_string();
            for index in 0..40 {
                let id = format!("m{index}");
                rows.push(format!(r#"{{"type":"message","id":"{id}","parentId":{parent},"message":{{"role":"user","content":"row-{index}"}}}}"#));
                parent = format!("\"{id}\"");
            }
            std::fs::write(path, format!("{}\n", rows.join("\n"))).unwrap();
            let transcript = TranscriptRef::new("pi", path).unwrap();
            let mut small =
                ConversationReader::new(crate::detect::Agent::Pi, "session".into(), "engine-a", 1);
            let mut large =
                ConversationReader::new(crate::detect::Agent::Pi, "session".into(), "engine-a", 1);
            let first_small = small.read(&transcript, None, ConversationPageDirection::Newest, 3);
            let first_large = large.read(&transcript, None, ConversationPageDirection::Newest, 10);
            assert!(
                first_small.page.is_some(),
                "reader failed reset={} reason={:?}",
                first_small.reset,
                first_small.capability_reason
            );
            let small_ids = first_small
                .page
                .as_ref()
                .unwrap()
                .items
                .iter()
                .map(|item| item.id.clone())
                .collect::<Vec<_>>();
            let large_ids = first_large
                .page
                .as_ref()
                .unwrap()
                .items
                .iter()
                .map(|item| item.id.clone())
                .collect::<Vec<_>>();
            assert_eq!(small_ids, large_ids[large_ids.len() - 3..]);
            let mut cursor = first_small.page.unwrap().previous_cursor;
            let mut older_count = 0;
            while let Some(next) = cursor {
                let page = small
                    .read(
                        &transcript,
                        Some(&next),
                        ConversationPageDirection::Older,
                        3,
                    )
                    .page
                    .unwrap();
                older_count += page.items.len();
                cursor = page.previous_cursor;
            }
            assert_eq!(older_count, 37);

            let newer_cursor = first_large.page.unwrap().next_cursor.unwrap();
            let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
            use std::io::Write;
            writeln!(file, "{{\"type\":\"message\",\"id\":\"m40\",\"parentId\":\"m39\",\"message\":{{\"role\":\"user\",\"content\":\"row-40\"}}}}").unwrap();
            let delta = large
                .read(
                    &transcript,
                    Some(&newer_cursor),
                    ConversationPageDirection::Newer,
                    10,
                )
                .page
                .unwrap();
            assert_eq!(delta.items.len(), 1);
            assert!(
                matches!(delta.items[0].payload, ConversationItemPayload::UserMessage { ref text, .. } if text == "row-40")
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn branch_switch_resets_instead_of_leaving_abandoned_items() {
        with_pi_fixture(|path| {
            std::fs::write(path, concat!(
                "{\"type\":\"message\",\"id\":\"root\",\"parentId\":null,\"message\":{\"role\":\"user\",\"content\":\"root\"}}\n",
                "{\"type\":\"message\",\"id\":\"old\",\"parentId\":\"root\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"abandoned\"}],\"stopReason\":\"stop\"}}\n",
            )).unwrap();
            let transcript = TranscriptRef::new("pi", path).unwrap();
            let mut reader = ConversationReader::new(
                crate::detect::Agent::Pi,
                "session".into(),
                "engine-branch",
                1,
            );
            let first = reader
                .read(&transcript, None, ConversationPageDirection::Newest, 10)
                .page
                .unwrap();
            let cursor = first.next_cursor.unwrap();
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
            writeln!(file, "{{\"type\":\"message\",\"id\":\"new\",\"parentId\":\"root\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"active\"}}],\"stopReason\":\"stop\"}}}}").unwrap();
            let outcome = reader.read(
                &transcript,
                Some(&cursor),
                ConversationPageDirection::Newer,
                10,
            );
            assert!(outcome.reset);
            let page = reader
                .read(&transcript, None, ConversationPageDirection::Newest, 10)
                .page
                .unwrap();
            assert!(page.items.iter().any(|item| matches!(item.payload, ConversationItemPayload::AssistantMessage { ref text, .. } if text == "active")));
            assert!(!page.items.iter().any(|item| matches!(item.payload, ConversationItemPayload::AssistantMessage { ref text, .. } if text == "abandoned")));
        });
    }

    #[test]
    fn overlay_reconciliation_is_single_item_and_revision_visible() {
        let mut state = test_state();
        let overlay = OverlayRecord {
            native_id: Some("call-1".into()),
            entry_id: Some("entry-1".into()),
            timestamp_ms: None,
            turn_id: Some("entry-1".into()),
            payload: ConversationItemPayload::ToolActivity {
                action: "edit".into(),
                label: "running".into(),
                status: ToolStatus::Running,
                preview: Some("cargo test".into()),
                detail: None,
                duration_ms: None,
                paths: vec!["src/chat.ts".into()],
            },
        };
        append_overlay(&mut state, "session", &overlay);
        let before = state.content_revision;
        append_durable(
            &mut state,
            "session",
            &NativeRecord {
                native_id: Some("call-1".into()),
                entry_id: Some("entry-1".into()),
                parent_id: None,
                timestamp_ms: None,
                turn_id: Some("entry-1".into()),
                topology_only: false,
                payload: ConversationItemPayload::ToolActivity {
                    action: "edit".into(),
                    label: "done".into(),
                    status: ToolStatus::Completed,
                    preview: None,
                    detail: None,
                    duration_ms: Some(2),
                    paths: Vec::new(),
                },
                anchor: 4,
            },
            4,
        );
        assert_eq!(state.log.len(), 2);
        assert!(!state.log[0].overlay);
        assert!(matches!(
            &state.log[0].payload,
            ConversationItemPayload::ToolActivity { paths, .. }
                if paths == &["src/chat.ts"]
        ));
        assert!(matches!(
            &state.log[0].payload,
            ConversationItemPayload::ToolActivity { preview: Some(preview), .. }
                if preview == "cargo test"
        ));
        assert!(state.log.iter().any(|entry| matches!(
            &entry.payload,
            ConversationItemPayload::FileChange { path, .. } if path == "src/chat.ts"
        )));
        assert!(state.content_revision > before);
    }

    #[test]
    fn durable_reconciliation_adopts_the_provider_entry_identity() {
        let mut state = test_state();
        append_overlay(
            &mut state,
            "session",
            &OverlayRecord {
                native_id: Some("user:turn-1".into()),
                entry_id: Some("turn-1".into()),
                timestamp_ms: Some(1),
                turn_id: Some("turn-1".into()),
                payload: ConversationItemPayload::UserMessage {
                    text: "hello".into(),
                    attachments: Vec::new(),
                },
            },
        );

        append_durable(
            &mut state,
            "session",
            &NativeRecord {
                native_id: Some("user:turn-1".into()),
                entry_id: Some("provider-user-1".into()),
                parent_id: Some("provider-parent".into()),
                timestamp_ms: Some(1),
                turn_id: Some("turn-1".into()),
                topology_only: false,
                payload: ConversationItemPayload::UserMessage {
                    text: "hello".into(),
                    attachments: Vec::new(),
                },
                anchor: 4,
            },
            4,
        );

        assert_eq!(state.log.len(), 1);
        assert_eq!(state.log[0].entry_id.as_deref(), Some("provider-user-1"));
        assert!(!state.log[0].overlay);
    }

    #[test]
    fn late_overlay_enriches_a_durable_tool_without_downgrading_its_state() {
        let mut state = test_state();
        append_durable(
            &mut state,
            "session",
            &NativeRecord {
                native_id: Some("call-1".into()),
                entry_id: Some("entry-1".into()),
                parent_id: None,
                timestamp_ms: None,
                turn_id: Some("entry-1".into()),
                topology_only: false,
                payload: ConversationItemPayload::ToolActivity {
                    action: "edit".into(),
                    label: "completed".into(),
                    status: ToolStatus::Completed,
                    preview: None,
                    detail: None,
                    duration_ms: Some(2),
                    paths: Vec::new(),
                },
                anchor: 4,
            },
            4,
        );
        let before = state.content_revision;

        append_overlay(
            &mut state,
            "session",
            &OverlayRecord {
                native_id: Some("call-1".into()),
                entry_id: Some("entry-1".into()),
                timestamp_ms: None,
                turn_id: Some("entry-1".into()),
                payload: ConversationItemPayload::ToolActivity {
                    action: "edit".into(),
                    label: "edit".into(),
                    status: ToolStatus::Running,
                    preview: Some("cargo test".into()),
                    detail: None,
                    duration_ms: None,
                    paths: vec!["src/chat.ts".into()],
                },
            },
        );

        assert_eq!(state.log.len(), 2);
        assert!(matches!(
            &state.log[0].payload,
            ConversationItemPayload::ToolActivity { status, paths, .. }
                if *status == ToolStatus::Completed && paths == &["src/chat.ts"]
        ));
        assert!(matches!(
            &state.log[0].payload,
            ConversationItemPayload::ToolActivity { preview: Some(preview), .. }
                if preview == "cargo test"
        ));
        assert!(state.log.iter().any(|entry| matches!(
            &entry.payload,
            ConversationItemPayload::FileChange { path, .. } if path == "src/chat.ts"
        )));
        assert!(state.content_revision > before);
    }

    #[test]
    fn malformed_utf8_and_oversized_rows_do_not_block_following_records() {
        #[derive(Debug)]
        struct AnyAdapter;
        impl ProviderAdapter for AnyAdapter {
            fn provider_name(&self) -> &'static str {
                "codex"
            }
            fn validate_source(&self, _path: &Path) -> Result<SourceFingerprint, TranscriptError> {
                unreachable!()
            }
            fn normalize_line(&self, line: &str) -> Vec<NativeRecord> {
                vec![NativeRecord {
                    native_id: Some(line.to_string()),
                    entry_id: Some(line.to_string()),
                    parent_id: None,
                    timestamp_ms: None,
                    turn_id: None,
                    topology_only: false,
                    payload: ConversationItemPayload::Notice {
                        message: cap_text(line, 16),
                    },
                    anchor: 0,
                }]
            }
        }
        let base = std::env::temp_dir().join(format!("herdr-malformed-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let path = base.join("rows.jsonl");
        let mut bytes = vec![b'x'; MAX_RECORD_BYTES + 5];
        bytes.push(b'\n');
        bytes.extend_from_slice(&[0xff, 0xfe, b'\n']);
        bytes.extend_from_slice(b"good\n");
        std::fs::write(&path, bytes).unwrap();
        let mut state =
            SourceReaderState::new(fingerprint_for(&path).unwrap(), ReaderLimits::production());
        jsonl::scan_for_page(
            &path,
            &mut state,
            &AnyAdapter,
            ConversationPageDirection::Newest,
            None,
            "s",
            None,
        )
        .unwrap();
        assert!(state
            .log
            .iter()
            .any(|entry| entry.native_id.as_deref() == Some("good")));
        let _ = std::fs::remove_dir_all(base);
    }
    #[cfg(unix)]
    #[test]
    fn provider_roots_reject_cross_provider_and_symlink_escape() {
        use std::os::unix::fs::symlink;
        let _guard = crate::integration::integration_env_lock();
        let base = std::env::temp_dir().join(format!(
            "herdr-conversation-security-{}",
            std::process::id()
        ));
        let pi_root = base.join("pi");
        let codex_root = base.join("codex");
        let outside = base.join("outside");
        std::fs::create_dir_all(&pi_root).unwrap();
        std::fs::create_dir_all(&codex_root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let pi_file = pi_root.join("session.jsonl");
        let codex_file = codex_root.join("session.jsonl");
        let outside_file = outside.join("session.jsonl");
        std::fs::write(&pi_file, b"{}\n").unwrap();
        std::fs::write(&codex_file, b"{}\n").unwrap();
        std::fs::write(&outside_file, b"{}\n").unwrap();
        let link = pi_root.join("escape.jsonl");
        symlink(&outside_file, &link).unwrap();
        std::env::set_var("PI_CODING_AGENT_DIR", &pi_root);
        std::env::set_var("CODEX_HOME", &codex_root);
        let pi_roots = provider_roots("pi");
        let refs: Vec<_> = pi_roots.iter().map(PathBuf::as_path).collect();
        assert!(validate_under_root(&pi_file, &refs).is_ok());
        assert!(validate_under_root(&codex_file, &refs).is_err());
        assert!(validate_under_root(&link, &refs).is_err());
        assert!(validate_under_root(&outside_file, &refs).is_err());
        assert!(validate_under_root(Path::new("relative.jsonl"), &refs).is_err());
        std::env::remove_var("PI_CODING_AGENT_DIR");
        std::env::remove_var("CODEX_HOME");
        let _ = std::fs::remove_dir_all(base);
    }

    #[cfg(unix)]
    #[test]
    fn replacing_a_source_at_the_same_path_changes_identity() {
        let base =
            std::env::temp_dir().join(format!("herdr-conversation-replace-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let path = base.join("session.jsonl");
        std::fs::write(&path, b"first\n").unwrap();
        let first = fingerprint_for(&path).unwrap().identity_token;
        let replacement = base.join("replacement.jsonl");
        std::fs::write(&replacement, b"second\n").unwrap();
        std::fs::rename(replacement, &path).unwrap();
        let second = fingerprint_for(&path).unwrap().identity_token;
        assert_ne!(first, second);
        let _ = std::fs::remove_dir_all(base);
    }
}

#[cfg(test)]
mod phase3_red_tests {
    use super::*;

    #[test]
    fn public_cursors_are_bounded_opaque_registry_handles() {
        let mut registry = CursorRegistry::new(1);
        let first = registry.issue(CursorState {
            generation: "g1".into(),
            session: "s1".into(),
            source_identity: "source".into(),
            direction: ConversationPageDirection::Older,
            sequence: 3,
            revision: 7,
            durable_anchor: 11,
        });
        assert!(first.len() >= 32);
        assert!(!first.contains("g1"));
        assert_eq!(registry.resolve(&first).unwrap().sequence, 3);

        let second = registry.issue(CursorState {
            sequence: 4,
            ..registry
                .resolve(&first)
                .unwrap_or_else(|_| panic!("state missing"))
                .clone()
        });
        assert_ne!(first, second);
        assert!(matches!(
            registry.resolve(&first),
            Err(CursorError::Evicted)
        ));
    }
}
#[cfg(test)]
mod regression_tests {
    use super::*;
    use crate::agent_conversation::tests::{test_state, with_pi_fixture};
    use std::collections::VecDeque;
    #[test]
    fn newer_pages_advance_in_change_order_without_skipping_reverse_updates() {
        let mut state = test_state();
        let user = |id: &str, text: &str| NativeRecord {
            native_id: Some(id.into()),
            entry_id: Some(id.into()),
            parent_id: None,
            timestamp_ms: None,
            turn_id: None,
            payload: ConversationItemPayload::UserMessage {
                text: text.into(),
                attachments: Vec::new(),
            },
            topology_only: false,
            anchor: 0,
        };
        append_durable(&mut state, "s", &user("u1", "one"), 1);
        append_durable(&mut state, "s", &user("u2", "two"), 2);
        let cursor = CursorState {
            generation: "g".into(),
            session: "s".into(),
            source_identity: "source".into(),
            direction: ConversationPageDirection::Newer,
            sequence: state.max_sequence,
            revision: state.content_revision,
            durable_anchor: 2,
        };
        append_durable(&mut state, "s", &user("u2", "two revised"), 3);
        append_durable(&mut state, "s", &user("u1", "one revised"), 4);
        let selected = select_page(&state, Some(&cursor), ConversationPageDirection::Newer, 1);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].native_id.as_deref(), Some("u2"));
    }

    #[test]
    fn newer_page_selection_sorts_retained_updates_by_revision_before_limiting() {
        let mut state = test_state();
        let user = |id: &str, text: &str| NativeRecord {
            native_id: Some(id.into()),
            entry_id: Some(id.into()),
            parent_id: None,
            timestamp_ms: None,
            turn_id: None,
            payload: ConversationItemPayload::UserMessage {
                text: text.into(),
                attachments: Vec::new(),
            },
            topology_only: false,
            anchor: 0,
        };
        append_durable(&mut state, "s", &user("u1", "one"), 1);
        append_durable(&mut state, "s", &user("u2", "two"), 2);
        let cursor = CursorState {
            generation: "g".into(),
            session: "s".into(),
            source_identity: "source".into(),
            direction: ConversationPageDirection::Newer,
            sequence: state.max_sequence,
            revision: 0,
            durable_anchor: 2,
        };
        let u1 = state.log.front().map(|entry| entry.sequence).unwrap();
        let u2 = state.log.back().map(|entry| entry.sequence).unwrap();
        state.change_log = VecDeque::from([(3, u1), (2, u2)]);
        let first = select_page(&state, Some(&cursor), ConversationPageDirection::Newer, 1);
        assert_eq!(first[0].native_id.as_deref(), Some("u2"));
        let second_cursor = CursorState {
            revision: 2,
            ..cursor
        };
        let second = select_page(
            &state,
            Some(&second_cursor),
            ConversationPageDirection::Newer,
            1,
        );
        assert_eq!(second[0].native_id.as_deref(), Some("u1"));
    }

    #[cfg(unix)]
    #[test]
    fn identity_repair_resets_so_desktop_never_keeps_ghost_duplicates() {
        with_pi_fixture(|path| {
            let u1 = serde_json::json!({
                "type": "message",
                "id": "u1",
                "parentId": serde_json::Value::Null,
                "message": {"role": "user", "timestamp": 1700000000000u64, "content": "hello"},
            })
            .to_string();
            let mut a1_text = String::from("done");
            while a1_text.len() < 190 {
                a1_text.push('x');
            }
            let a1 = serde_json::json!({
                "type": "message",
                "id": "a1",
                "parentId": "u1",
                "message": {"role": "assistant", "timestamp": 1700000000001u64, "content": [{"type": "text", "text": a1_text}], "stopReason": "stop"},
            })
            .to_string();
            std::fs::write(path, format!("{u1}\n{a1}\n")).unwrap();
            let transcript = TranscriptRef::new("pi", path).unwrap();
            let mut limits = ReaderLimits::test_small();
            limits.max_scan_bytes = a1.len() as u64 + 4;
            limits.max_items_per_page = 16;
            limits.max_page_bytes = 64 * 1024;
            let mut reader = ConversationReader::new_with_limits(
                crate::detect::Agent::Pi,
                "session".into(),
                "engine-ghost-duplicate",
                1,
                limits,
            );
            assert!(reader.accept_overlay(OverlayRecord {
                native_id: Some("message:turn:1700000000000:1700000000001".into()),
                entry_id: None,
                timestamp_ms: Some(1700000000001),
                turn_id: Some("turn:1700000000000".into()),
                payload: ConversationItemPayload::AssistantMessage {
                    phase: crate::api::schema::conversations::AssistantMessagePhase::Final,
                    text: "live".into(),
                    state: crate::api::schema::conversations::CompletionState::Completed,
                },
            }));
            let newest = reader
                .read(&transcript, None, ConversationPageDirection::Newest, 16)
                .page
                .unwrap();
            assert_eq!(
                newest.items.len(),
                2,
                "durable provider item and live overlay are both visible before repair"
            );
            let older = reader.read(
                &transcript,
                newest.previous_cursor.as_deref(),
                ConversationPageDirection::Older,
                16,
            );
            assert!(
                older.reset,
                "identity repair must force a reset so Desktop drops the ghost duplicate"
            );
            let rebuilt = reader
                .read(&transcript, None, ConversationPageDirection::Newest, 16)
                .page
                .unwrap();
            assert_eq!(rebuilt.items.len(), 2);
            assert_eq!(
                rebuilt
                    .items
                    .iter()
                    .filter(|item| matches!(
                        item.payload,
                        ConversationItemPayload::AssistantMessage { .. }
                    ))
                    .count(),
                1
            );
            let accepted = reader.accept_overlay(OverlayRecord {
                native_id: Some("message:turn:1700000000000:1700000000001".into()),
                entry_id: None,
                timestamp_ms: Some(1700000000002),
                turn_id: Some("turn:1700000000000".into()),
                payload: ConversationItemPayload::AssistantMessage {
                    phase: crate::api::schema::conversations::AssistantMessagePhase::Final,
                    text: "live again".into(),
                    state: crate::api::schema::conversations::CompletionState::Completed,
                },
            });
            let refreshed = reader
                .read(&transcript, None, ConversationPageDirection::Newest, 16)
                .page
                .unwrap();
            assert_eq!(refreshed.items.len(), 2);
            let assistant = refreshed
                .items
                .iter()
                .find(|item| {
                    matches!(
                        item.payload,
                        ConversationItemPayload::AssistantMessage { .. }
                    )
                })
                .unwrap();
            if accepted {
                assert!(matches!(
                    &assistant.payload,
                    ConversationItemPayload::AssistantMessage { text, .. } if text == "live again"
                ));
            }
        });
    }

    #[cfg(unix)]
    #[test]
    fn bounded_topology_preserves_active_branch_history_without_leaking_abandoned_items() {
        with_pi_fixture(|path| {
            std::fs::write(
                path,
                concat!(
                    "{\"type\":\"message\",\"id\":\"root\",\"parentId\":null,\"message\":{\"role\":\"user\",\"content\":\"root\"}}\n",
                    "{\"type\":\"message\",\"id\":\"old-1\",\"parentId\":\"root\",\"message\":{\"role\":\"user\",\"content\":\"abandoned one\"}}\n",
                    "{\"type\":\"message\",\"id\":\"old-2\",\"parentId\":\"old-1\",\"message\":{\"role\":\"user\",\"content\":\"abandoned two\"}}\n",
                    "{\"type\":\"message\",\"id\":\"active-1\",\"parentId\":\"root\",\"message\":{\"role\":\"user\",\"content\":\"active one\"}}\n",
                    "{\"type\":\"message\",\"id\":\"active-2\",\"parentId\":\"active-1\",\"message\":{\"role\":\"user\",\"content\":\"active two\"}}\n",
                    "{\"type\":\"message\",\"id\":\"active-3\",\"parentId\":\"active-2\",\"message\":{\"role\":\"user\",\"content\":\"active three\"}}\n",
                ),
            )
            .unwrap();
            let transcript = TranscriptRef::new("pi", path).unwrap();
            let mut limits = ReaderLimits::test_small();
            limits.max_log_entries = 4;
            limits.max_topology_entries = 2;
            limits.max_scan_bytes = 4096;
            limits.max_items_per_page = 32;
            limits.max_page_bytes = 16 * 1024;
            let mut reader = ConversationReader::new_with_limits(
                crate::detect::Agent::Pi,
                "session".into(),
                "engine-topology-branch-retention",
                1,
                limits,
            );
            let page = reader
                .read(&transcript, None, ConversationPageDirection::Newest, 32)
                .page
                .unwrap();
            let texts = page
                .items
                .iter()
                .filter_map(|item| match &item.payload {
                    ConversationItemPayload::UserMessage { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(texts, ["root", "active one", "active two", "active three"]);
        });
    }

    #[cfg(unix)]
    #[test]
    fn older_paging_progresses_past_an_oversized_row_at_the_window_boundary() {
        with_pi_fixture(|path| {
            let row = |id: &str, parent: serde_json::Value, content: String| {
                serde_json::json!({
                    "type": "message",
                    "id": id,
                    "parentId": parent,
                    "message": {"role": "user", "content": content},
                })
                .to_string()
            };
            let history = row("h1", serde_json::Value::Null, "history".into());
            let fixed = row("big", serde_json::json!("h1"), String::new());
            let content_len = 500usize.saturating_sub(fixed.len());
            let oversized = row("big", serde_json::json!("h1"), "x".repeat(content_len));
            assert_eq!(
                oversized.len() + 1,
                501,
                "oversized row must land on the scan-window boundary"
            );
            let tail = row("t1", serde_json::json!("big"), "tail".into());
            std::fs::write(path, format!("{history}\n{oversized}\n{tail}\n")).unwrap();
            let transcript = TranscriptRef::new("pi", path).unwrap();
            let mut limits = ReaderLimits::test_small();
            limits.max_scan_bytes = 500;
            limits.max_items_per_page = 16;
            limits.max_page_bytes = 64 * 1024;
            let mut reader = ConversationReader::new_with_limits(
                crate::detect::Agent::Pi,
                "session".into(),
                "engine-oversized-older",
                1,
                limits,
            );
            let text_of = |item: &ConversationItem| match &item.payload {
                ConversationItemPayload::UserMessage { text, .. } => {
                    if text.starts_with('x') {
                        Some("big".to_string())
                    } else {
                        Some(text.clone())
                    }
                }
                _ => None,
            };
            let mut seen = Vec::new();
            let newest = reader
                .read(&transcript, None, ConversationPageDirection::Newest, 16)
                .page
                .unwrap();
            seen.extend(newest.items.iter().filter_map(text_of));
            let mut cursor = newest.previous_cursor;
            let mut guard = 0;
            while let Some(next_cursor) = cursor {
                guard += 1;
                assert!(
                    guard < 20,
                    "older paging must terminate past the oversized row"
                );
                let outcome = reader.read(
                    &transcript,
                    Some(&next_cursor),
                    ConversationPageDirection::Older,
                    16,
                );
                assert!(!outcome.reset);
                let page = outcome.page.unwrap();
                seen.extend(page.items.iter().filter_map(text_of));
                cursor = page.previous_cursor;
            }
            seen.sort();
            assert_eq!(seen, ["big", "history", "tail"]);
        });
    }

    #[cfg(unix)]
    #[test]
    fn dense_older_window_returns_every_active_ancestor_beyond_the_cap() {
        with_pi_fixture(|path| {
            let row = |id: &str, parent: serde_json::Value, text: &str, pad: usize| {
                let mut content = format!("{text}:");
                while content.len() < pad {
                    content.push('x');
                }
                serde_json::json!({
                    "type": "message",
                    "id": id,
                    "parentId": parent,
                    "message": {"role": "user", "content": content},
                })
                .to_string()
            };
            let rows = [
                row("u0", serde_json::Value::Null, "u0", 6),
                row("b1", serde_json::json!("u0"), "abandoned", 6),
                row("a1", serde_json::json!("u0"), "a1", 6),
                row("a2", serde_json::json!("a1"), "a2", 6),
                row("a3", serde_json::json!("a2"), "a3", 6),
                row("a4", serde_json::json!("a3"), "a4", 6),
                row("a5", serde_json::json!("a4"), "a5", 6),
                row("a6", serde_json::json!("a5"), "a6", 400),
            ];
            std::fs::write(path, format!("{}\n", rows.join("\n"))).unwrap();
            let transcript = TranscriptRef::new("pi", path).unwrap();
            let mut limits = ReaderLimits::test_small();
            limits.max_topology_entries = 16;
            limits.max_scan_bytes = 500;
            limits.max_items_per_page = 16;
            limits.max_page_bytes = 64 * 1024;
            let mut reader = ConversationReader::new_with_limits(
                crate::detect::Agent::Pi,
                "session".into(),
                "engine-dense-window",
                1,
                limits,
            );
            let text_of = |item: &ConversationItem| match &item.payload {
                ConversationItemPayload::UserMessage { text, .. } => {
                    text.split(':').next().map(str::to_string)
                }
                _ => None,
            };
            let mut seen = Vec::new();
            let newest = reader
                .read(&transcript, None, ConversationPageDirection::Newest, 16)
                .page
                .unwrap();
            seen.extend(newest.items.iter().filter_map(text_of));
            let mut cursor = newest.previous_cursor;
            let mut guard = 0;
            while let Some(next_cursor) = cursor {
                guard += 1;
                assert!(guard < 20, "older paging must terminate");
                let outcome = reader.read(
                    &transcript,
                    Some(&next_cursor),
                    ConversationPageDirection::Older,
                    16,
                );
                assert!(!outcome.reset);
                let page = outcome.page.unwrap();
                seen.extend(page.items.iter().filter_map(text_of));
                cursor = page.previous_cursor;
            }
            seen.sort();
            assert_eq!(
                seen,
                ["a1", "a2", "a3", "a4", "a5", "a6", "u0"]
                    .into_iter()
                    .map(str::to_string)
                    .collect::<Vec<_>>(),
                "every active ancestor must be reachable even when one window exceeds the parent-index cap"
            );
            assert!(!seen.iter().any(|text| text.starts_with("abandoned")));
        });
    }

    #[cfg(unix)]
    #[test]
    fn older_paging_walks_active_ancestry_beyond_the_parent_index_cap() {
        with_pi_fixture(|path| {
            let row = |id: &str, parent: serde_json::Value, text: &str| {
                let mut content = format!("{text}:");
                while content.len() < 100 {
                    content.push('x');
                }
                serde_json::json!({
                    "type": "message",
                    "id": id,
                    "parentId": parent,
                    "message": {"role": "user", "content": content},
                })
                .to_string()
            };
            let rows = [
                row("root", serde_json::Value::Null, "root"),
                row("b1", serde_json::json!("root"), "abandoned"),
                row("a1", serde_json::json!("root"), "a1"),
                row("a2", serde_json::json!("a1"), "a2"),
                row("a3", serde_json::json!("a2"), "a3"),
                row("a4", serde_json::json!("a3"), "a4"),
                row("a5", serde_json::json!("a4"), "a5"),
            ];
            std::fs::write(path, format!("{}\n", rows.join("\n"))).unwrap();
            let transcript = TranscriptRef::new("pi", path).unwrap();
            let mut limits = ReaderLimits::test_small();
            limits.max_scan_bytes = 500;
            limits.max_items_per_page = 16;
            limits.max_page_bytes = 64 * 1024;
            let mut reader = ConversationReader::new_with_limits(
                crate::detect::Agent::Pi,
                "session".into(),
                "engine-older-bridge-walk",
                1,
                limits,
            );
            let text_of = |item: &ConversationItem| match &item.payload {
                ConversationItemPayload::UserMessage { text, .. } => {
                    text.split(':').next().map(str::to_string)
                }
                _ => None,
            };
            let mut seen = Vec::new();
            let newest = reader
                .read(&transcript, None, ConversationPageDirection::Newest, 16)
                .page
                .unwrap();
            seen.extend(newest.items.iter().filter_map(text_of));
            let mut cursor = newest.previous_cursor;
            while let Some(next_cursor) = cursor {
                let outcome = reader.read(
                    &transcript,
                    Some(&next_cursor),
                    ConversationPageDirection::Older,
                    16,
                );
                assert!(!outcome.reset);
                let page = outcome.page.unwrap();
                seen.extend(page.items.iter().filter_map(text_of));
                cursor = page.previous_cursor;
            }
            seen.sort();
            assert_eq!(
                seen,
                ["a1", "a2", "a3", "a4", "a5", "root"]
                    .into_iter()
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            );
            assert!(!seen.iter().any(|text| text.starts_with("abandoned")));
        });
    }
}
