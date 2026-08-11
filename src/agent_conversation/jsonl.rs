//! Provider-independent bounded byte-window mechanics.

use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use super::{
    append_durable, merge_payload, prepend_durable, refresh_overlay_bytes, remember_change,
    NativeRecord, ProviderAdapter, SourceReaderState, TranscriptError, MAX_RECORD_BYTES,
};
use crate::api::schema::conversations::ConversationPageDirection;
pub fn scan_for_page(
    path: &Path,
    state: &mut SourceReaderState,
    adapter: &dyn ProviderAdapter,
    direction: ConversationPageDirection,
    cursor: Option<&super::CursorState>,
    session: &str,
    display_root: Option<&Path>,
) -> Result<(), TranscriptError> {
    let mut source = open_source(path)?;
    let current = source.fingerprint.clone();
    if current.identity_token != state.fingerprint.identity_token {
        return Err(TranscriptError::TruncatedOrReplaced);
    }
    let size = current.size;
    if matches!(
        direction,
        ConversationPageDirection::Newest | ConversationPageDirection::Newer
    ) && (state.tail_offset == 0 || size > state.tail_offset)
    {
        if state.tail_offset == 0 {
            // Initial tail window read from EOF. This must not depend on the
            // canonical log being empty: live overlays are accepted before
            // the first read and already occupy the log.
            let start = size.saturating_sub(state.limits.max_scan_bytes);
            let window = read_window(
                &mut source.file,
                start,
                size,
                adapter,
                true,
                state.limits.max_scan_bytes,
                size,
                display_root,
            )?;
            let records = ingest_provider_records(state, adapter, window.records, true);
            for record in records {
                let anchor = record.anchor;
                append_durable(state, session, &record, anchor);
            }
            state.tail_offset = window.complete_end;
            state.tail_at_boundary = window.next_tail_boundary;
            state.older_anchor = window.older_anchor;
            state.more_older = state.older_anchor > 0;
        } else if size > state.tail_offset {
            let end = (state.tail_offset + state.limits.max_scan_bytes).min(size);
            let window = read_window(
                &mut source.file,
                state.tail_offset,
                end,
                adapter,
                false,
                state.limits.max_scan_bytes,
                size,
                display_root,
            )?;
            let records = ingest_provider_records(state, adapter, window.records, true);
            for record in records {
                let anchor = record.anchor;
                append_durable(state, session, &record, anchor);
            }
            state.tail_offset = window.complete_end.max(state.tail_offset);
            state.tail_at_boundary = window.next_tail_boundary;
        }
    }
    if matches!(direction, ConversationPageDirection::Older) {
        // End at the cursor's line-start anchor (no +1). The window starts one
        // byte earlier so a row that ends exactly at the anchor (whose total
        // length can be max_scan + 1) is fully contained; read_window skips
        // the leading fragment only when start is genuinely mid-record, so a
        // row starting exactly at a boundary is parsed instead of dropped.
        // Rows at/after the anchor are already ingested, so re-reading them is
        // idempotent.
        let end = cursor
            .map(|cursor| cursor.durable_anchor)
            .filter(|anchor| *anchor > 0)
            .unwrap_or(state.older_anchor);
        if end == 0 {
            state.fingerprint = current;
            return Ok(());
        }
        let max_scan = state.limits.max_scan_bytes.saturating_add(1);
        let start = end.saturating_sub(max_scan);
        let window = read_window(
            &mut source.file,
            start,
            end,
            adapter,
            true,
            max_scan,
            size,
            display_root,
        )?;
        let records = ingest_provider_records(state, adapter, window.records, false);
        for record in records.into_iter().rev() {
            let anchor = record.anchor;
            prepend_durable(state, session, &record, anchor);
        }
        state.older_anchor = if state.older_anchor == 0 {
            window.older_anchor
        } else {
            state.older_anchor.min(window.older_anchor)
        };
        state.more_older = state.older_anchor > 0;
    }
    state.fingerprint = current;
    Ok(())
}

fn ingest_provider_records(
    state: &mut SourceReaderState,
    adapter: &dyn ProviderAdapter,
    mut records: Vec<NativeRecord>,
    update_tip: bool,
) -> Vec<NativeRecord> {
    let tree_provider = matches!(adapter.provider_name(), "pi" | "omp");
    let previous_tip = state.latest_entry_id.clone();
    let mut batch_branching = false;
    let mut batch_active_nodes = Vec::new();
    if tree_provider {
        let mut child_by_parent = HashMap::new();
        for record in &records {
            let (Some(entry_id), Some(parent_id)) =
                (record.entry_id.as_ref(), record.parent_id.as_ref())
            else {
                continue;
            };
            if child_by_parent
                .insert(parent_id.clone(), entry_id.clone())
                .is_some_and(|existing| existing != *entry_id)
            {
                batch_branching = true;
            }
        }
        let batch_tip = records
            .iter()
            .rev()
            .find_map(|record| record.entry_id.clone())
            .or_else(|| state.latest_entry_id.clone());
        let batch_topology = records
            .iter()
            .filter(|record| record.entry_id.is_some())
            .cloned()
            .map(|mut record| {
                record.native_id = None;
                record.topology_only = true;
                record
            })
            .collect::<Vec<_>>();
        batch_active_nodes =
            adapter.select_active_branch_from_tip(batch_topology, batch_tip.as_deref());
    }
    state.topology_branching |= batch_branching;
    if update_tip {
        if let Some(tip) = records
            .iter()
            .rev()
            .find_map(|record| record.entry_id.clone())
        {
            state.latest_entry_id = Some(tip);
        }
    }

    let mut branch_from_existing = false;
    for record in &records {
        let Some(entry_id) = record.entry_id.clone() else {
            continue;
        };
        if tree_provider {
            if let Some(parent_id) = record.parent_id.as_deref() {
                if state
                    .entry_parents
                    .iter()
                    .any(|(existing_id, existing_parent)| {
                        existing_id != &entry_id && existing_parent.as_deref() == Some(parent_id)
                    })
                {
                    state.topology_branching = true;
                    branch_from_existing = true;
                }
            }
        }
        remember_entry_parent(state, entry_id.clone(), record.parent_id.clone());
        if matches!(
            record.payload,
            crate::api::schema::conversations::ConversationItemPayload::UserMessage { .. }
        ) {
            state.entry_turns.insert(
                entry_id.clone(),
                record.turn_id.clone().unwrap_or_else(|| entry_id.clone()),
            );
        }
        if let Some(turn_id) = record.turn_id.as_ref() {
            state.entry_turns.insert(entry_id, turn_id.clone());
        }
    }

    if tree_provider {
        for record in &records {
            let Some(entry_id) = record.entry_id.clone() else {
                continue;
            };
            if let Some(existing) = state
                .topology
                .iter_mut()
                .find(|existing| existing.entry_id.as_deref() == Some(entry_id.as_str()))
            {
                existing.parent_id = record.parent_id.clone();
                existing.timestamp_ms = record.timestamp_ms.or(existing.timestamp_ms);
            } else {
                let mut node = record.clone();
                node.native_id = None;
                node.topology_only = true;
                state.topology.push(node);
            }
        }
        while state.topology.len() > state.limits.max_topology_entries {
            state.topology.remove(0);
            state.topology_truncated = true;
        }
    }

    refresh_missing_entry_turns(state);
    for record in &mut records {
        if update_tip {
            if let Some(turn_id) = record.turn_id.as_ref() {
                state.active_turn_id = Some(turn_id.clone());
            }
        }
        if record.turn_id.is_none() {
            if let Some(entry_id) = record.entry_id.clone() {
                let mut current = entry_id;
                for _ in 0..state.limits.max_log_entries {
                    if let Some(turn) = state.entry_turns.get(&current).cloned() {
                        record.turn_id = Some(turn);
                        break;
                    }
                    let Some(parent) = state.entry_parents.get(&current).cloned().flatten() else {
                        break;
                    };
                    current = parent;
                }
            }
        }
        if record.turn_id.is_none() && update_tip {
            record.turn_id = state.active_turn_id.clone();
        }
        if update_tip {
            state.active_turn_id = record.turn_id.clone().or(state.active_turn_id.clone());
        }
        if let (Some(entry_id), Some(turn_id)) = (record.entry_id.as_ref(), record.turn_id.as_ref())
        {
            state.entry_turns.insert(entry_id.clone(), turn_id.clone());
        }
        if tree_provider {
            canonicalize_record_identity(state, record);
        }
    }

    if tree_provider {
        refresh_retained_entry_turns(state);
    }
    if !tree_provider {
        return records;
    }

    // Active-branch selection uses the bounded parent index rather than the
    // separately bounded topology evidence window. The latter may evict the
    // root of a long active branch even though the canonical log still holds
    // that history. Parent-index eviction prefers non-active nodes, so the
    // active ancestry remains available up to the same bound as renderable
    // canonical entries.
    let active_topology = state
        .entry_parent_order
        .iter()
        .filter_map(|entry_id| {
            state
                .entry_parents
                .get(entry_id)
                .map(|parent_id| NativeRecord {
                    native_id: None,
                    entry_id: Some(entry_id.clone()),
                    parent_id: parent_id.clone(),
                    timestamp_ms: None,
                    turn_id: None,
                    payload: crate::api::schema::conversations::ConversationItemPayload::Notice {
                        message: String::new(),
                    },
                    topology_only: true,
                    anchor: 0,
                })
        })
        .collect::<Vec<_>>();
    let parent_active_nodes =
        adapter.select_active_branch_from_tip(active_topology, state.latest_entry_id.as_deref());
    let use_batch_active_nodes = update_tip && !batch_active_nodes.is_empty();
    let active_nodes = if use_batch_active_nodes {
        batch_active_nodes
    } else {
        parent_active_nodes
    };
    let active_entry_order: Vec<String> = active_nodes
        .iter()
        .filter_map(|record| record.entry_id.clone())
        .collect();
    let active_entry_ids: HashSet<_> = active_entry_order.iter().cloned().collect();
    let derived_ancestor_ids = active_nodes
        .iter()
        .filter_map(|record| record.parent_id.as_ref())
        .filter(|parent_id| !active_entry_ids.contains(*parent_id))
        .cloned()
        .collect::<HashSet<_>>();
    let active_branch_available = !active_entry_order.is_empty();
    if active_branch_available {
        if use_batch_active_nodes
            && active_nodes
                .first()
                .is_some_and(|record| record.parent_id.is_none())
        {
            state.active_branch_complete = true;
        } else if !state.active_branch_complete {
            state.active_branch_complete = active_nodes
                .first()
                .is_some_and(|record| record.parent_id.is_none());
        }
        state.active_ancestor_ids = if update_tip {
            derived_ancestor_ids.clone()
        } else {
            state.active_ancestor_ids.clone()
        };
    }
    let linear_continuation = previous_tip.as_deref().is_some_and(|tip| {
        records
            .iter()
            .any(|record| record.parent_id.as_deref() == Some(tip))
    });
    branch_from_existing |= batch_branching;
    let branch_changed = update_tip
        && !state.active_entry_ids.is_empty()
        && state
            .active_entry_ids
            .difference(&active_entry_ids)
            .next()
            .is_some()
        && (!linear_continuation || branch_from_existing);
    if branch_changed {
        state.branch_reset = true;
        for entry_id in state.active_entry_ids.difference(&active_entry_ids) {
            state.abandoned_entry_ids.insert(entry_id.clone());
        }
        while state.abandoned_entry_ids.len() > state.limits.max_topology_entries {
            if let Some(entry_id) = state.abandoned_entry_ids.iter().next().cloned() {
                state.abandoned_entry_ids.remove(&entry_id);
            } else {
                break;
            }
        }
        state.log.retain(|entry| {
            entry.overlay
                || entry
                    .entry_id
                    .as_ref()
                    .is_none_or(|id| active_entry_ids.contains(id))
        });
        state.content_revision = state.content_revision.saturating_add(1);
    }
    if active_branch_available {
        if branch_changed || state.active_entry_order.is_empty() {
            state.active_entry_order = active_entry_order.into_iter().collect();
        } else {
            for entry_id in active_entry_order {
                if !state.active_entry_order.contains(&entry_id) {
                    state.active_entry_order.push_back(entry_id);
                }
            }
        }
        while state.active_entry_order.len() > state.limits.max_log_entries {
            state.active_entry_order.pop_front();
        }
        state.active_entry_ids = state.active_entry_order.iter().cloned().collect();
    }

    let active_ids_for_filter = if active_branch_available {
        active_entry_ids
    } else {
        state.active_entry_ids.clone()
    };
    let active_ancestor_ids_for_filter = if active_branch_available {
        derived_ancestor_ids
    } else {
        state.active_ancestor_ids.clone()
    };
    // Older pages walk the missing-ancestor bridge transitively through the
    // complete byte window. The bounded parent index may have evicted active
    // ancestors, so the immediate missing parent alone would silently skip
    // the rest of an active branch longer than the cap. Walking the batch
    // parent map keeps every ancestor in this window and retains the next
    // bridge for the following page.
    let mut walked_ancestor_ids: HashSet<String> = HashSet::new();
    if !update_tip {
        let mut batch_parents: HashMap<&str, Option<&str>> = HashMap::new();
        for record in &records {
            if let (Some(entry_id), parent_id) =
                (record.entry_id.as_deref(), record.parent_id.as_deref())
            {
                batch_parents.insert(entry_id, parent_id);
            }
        }
        let bridges: Vec<String> = if state.active_ancestor_ids.is_empty() {
            active_ancestor_ids_for_filter.iter().cloned().collect()
        } else {
            state.active_ancestor_ids.iter().cloned().collect()
        };
        let mut next_bridge = HashSet::new();
        for bridge in bridges {
            let mut current = bridge;
            // Bound the walk by the complete byte-window parent map (with
            // cycle detection), never by the retained-log cap: a dense window
            // can contain more active ancestors than max_log_entries.
            let mut visited = HashSet::new();
            while visited.insert(current.clone()) {
                walked_ancestor_ids.insert(current.clone());
                let Some(parent) = batch_parents.get(current.as_str()).copied().flatten() else {
                    if batch_parents.contains_key(current.as_str()) {
                        // True root with a null parent: the active chain is
                        // complete inside this window.
                        break;
                    }
                    // Bridge not present in this window; keep it for the
                    // next older page.
                    next_bridge.insert(current.clone());
                    break;
                };
                if batch_parents.contains_key(parent) {
                    current = parent.to_string();
                } else if state.entry_parents.contains_key(parent) {
                    break;
                } else {
                    next_bridge.insert(parent.to_string());
                    break;
                }
            }
        }
        state.active_ancestor_ids = next_bridge;
    }
    let filter_to_active_branch = state.topology_branching && !active_ids_for_filter.is_empty();
    records
        .into_iter()
        .filter(|record| {
            !record.topology_only
                && record.entry_id.as_ref().is_some_and(|id| {
                    !state.abandoned_entry_ids.contains(id)
                        && (!filter_to_active_branch
                            || active_ids_for_filter.contains(id)
                            || active_ancestor_ids_for_filter.contains(id)
                            || walked_ancestor_ids.contains(id))
                })
        })
        .collect()
}

fn canonicalize_record_identity(state: &mut SourceReaderState, record: &mut NativeRecord) {
    let Some(turn_id) = record.turn_id.as_deref() else {
        return;
    };
    if let Some(native_id) = canonical_native_id_for_payload(
        state,
        record.entry_id.as_deref(),
        turn_id,
        record.timestamp_ms,
        &record.payload,
    ) {
        record.native_id = Some(native_id);
    }
    trim_canonical_identity_maps(state);
}

fn canonical_native_id_for_payload(
    state: &mut SourceReaderState,
    entry_id: Option<&str>,
    turn_id: &str,
    timestamp_ms: Option<u64>,
    payload: &crate::api::schema::conversations::ConversationItemPayload,
) -> Option<String> {
    match payload {
        crate::api::schema::conversations::ConversationItemPayload::UserMessage { .. } => {
            Some(format!("user:{turn_id}"))
        }
        crate::api::schema::conversations::ConversationItemPayload::AssistantMessage { .. } => {
            let entry_id = entry_id?;
            let key = format!("assistant:{entry_id}");
            if let Some(timestamp_ms) = timestamp_ms {
                let native_id = format!("message:{turn_id}:{timestamp_ms}");
                state.canonical_native_ids.insert(key, native_id.clone());
                Some(native_id)
            } else if let Some(native_id) = state.canonical_native_ids.get(&key) {
                Some(native_id.clone())
            } else {
                let ordinal = state
                    .message_ordinals
                    .entry(turn_id.to_string())
                    .or_insert(0);
                let native_id = format!("message:{turn_id}:{ordinal}");
                *ordinal = ordinal.saturating_add(1);
                state.canonical_native_ids.insert(key, native_id.clone());
                Some(native_id)
            }
        }
        crate::api::schema::conversations::ConversationItemPayload::PlanUpdate { .. } => {
            Some(format!("plan:{turn_id}"))
        }
        crate::api::schema::conversations::ConversationItemPayload::TurnState { .. } => {
            Some(format!("turn:{turn_id}"))
        }
        _ => None,
    }
}

fn trim_canonical_identity_maps(state: &mut SourceReaderState) {
    while state.canonical_native_ids.len() > state.limits.max_log_entries.saturating_mul(2) {
        if let Some(key) = state.canonical_native_ids.keys().next().cloned() {
            state.canonical_native_ids.remove(&key);
        } else {
            break;
        }
    }
    while state.message_ordinals.len() > state.limits.max_log_entries {
        if let Some(key) = state.message_ordinals.keys().next().cloned() {
            state.message_ordinals.remove(&key);
        } else {
            break;
        }
    }
}

fn refresh_missing_entry_turns(state: &mut SourceReaderState) {
    let entries = state.entry_parent_order.iter().cloned().collect::<Vec<_>>();
    for entry_id in entries {
        if state.entry_turns.contains_key(&entry_id) {
            continue;
        }
        let mut current = entry_id.clone();
        for _ in 0..state.limits.max_log_entries {
            let Some(parent) = state.entry_parents.get(&current).cloned().flatten() else {
                break;
            };
            current = parent;
            if let Some(turn_id) = state.entry_turns.get(&current).cloned() {
                state.entry_turns.insert(entry_id.clone(), turn_id);
                break;
            }
        }
    }
}

fn refresh_retained_entry_turns(state: &mut SourceReaderState) {
    refresh_missing_entry_turns(state);
    let mut changed_sequences = Vec::new();
    for index in 0..state.log.len() {
        let Some(entry_id) = state.log[index].entry_id.clone() else {
            continue;
        };
        let Some(turn_id) = state.entry_turns.get(&entry_id).cloned() else {
            continue;
        };
        let payload = state.log[index].payload.clone();
        let timestamp_ms = state.log[index].timestamp_ms;
        let native_id = canonical_native_id_for_payload(
            state,
            Some(&entry_id),
            &turn_id,
            timestamp_ms,
            &payload,
        );
        let entry = &mut state.log[index];
        let native_changed = native_id
            .as_ref()
            .is_some_and(|native_id| entry.native_id.as_ref() != Some(native_id));
        let changed = entry.turn_id.as_deref() != Some(turn_id.as_str()) || native_changed;
        if changed {
            entry.turn_id = Some(turn_id);
            if native_id.is_some() {
                entry.native_id = native_id;
            }
            entry.updated_revision = state.content_revision.saturating_add(1);
            changed_sequences.push(entry.sequence);
        }
    }
    if !changed_sequences.is_empty() {
        state.content_revision = state.content_revision.saturating_add(1);
        for sequence in changed_sequences {
            remember_change(state, sequence);
        }
        merge_duplicate_entries(state);
        refresh_overlay_bytes(state);
    }
    trim_canonical_identity_maps(state);
}

fn merge_duplicate_entries(state: &mut SourceReaderState) {
    let mut index = 0;
    let mut removed = false;
    while index < state.log.len() {
        let Some(native_id) = state.log[index].native_id.clone() else {
            index += 1;
            continue;
        };
        let Some(duplicate_index) = ((index + 1)..state.log.len()).find(|candidate| {
            state.log[*candidate].native_id.as_deref() == Some(native_id.as_str())
        }) else {
            index += 1;
            continue;
        };
        let Some(duplicate) = state.log.remove(duplicate_index) else {
            index += 1;
            continue;
        };
        removed = true;
        let revision = state.content_revision.saturating_add(1);
        let changed_sequence = if let Some(entry) = state.log.get_mut(index) {
            entry.payload = merge_payload(&entry.payload, &duplicate.payload);
            entry.entry_id = entry.entry_id.clone().or(duplicate.entry_id);
            entry.timestamp_ms = entry.timestamp_ms.or(duplicate.timestamp_ms);
            entry.turn_id = entry.turn_id.clone().or(duplicate.turn_id);
            entry.anchor = entry.anchor.max(duplicate.anchor);
            entry.overlay = entry.overlay && duplicate.overlay;
            // Re-key to the durable identity when the removed twin was the
            // durable entry, so the next tail rescan merges by canonical id
            // instead of recreating the provider-ID entry next to the live
            // overlay.
            if !duplicate.overlay {
                entry.id = duplicate.id;
            }
            entry.updated_revision = revision;
            Some(entry.sequence)
        } else {
            None
        };
        if let Some(sequence) = changed_sequence {
            state.content_revision = revision;
            remember_change(state, sequence);
        }
    }
    if removed {
        // A canonical entry the client may already have rendered disappeared
        // (live overlay collapsed into its repaired durable twin). Desktop
        // merges only by public item id and never removes stale items, so
        // force a one-shot reset_required: the client re-reads and ends with
        // one item instead of a ghost duplicate. The read path treats this as
        // non-destructive (repaired state survives).
        state.branch_reset = true;
        state.identity_collapse_reset = true;
    }
}

fn remember_entry_parent(
    state: &mut SourceReaderState,
    entry_id: String,
    parent_id: Option<String>,
) {
    let is_new = !state.entry_parents.contains_key(&entry_id);
    state.entry_parents.insert(entry_id.clone(), parent_id);
    if is_new {
        state.entry_parent_order.push_back(entry_id);
    }
    // Bounded by the topology-evidence budget (never the canonical-log cap):
    // older-page filtering needs sibling/ancestor evidence that can outlive
    // the retained canonical window, and evicting it lets abandoned rows leak.
    while state.entry_parent_order.len() > state.limits.max_topology_entries {
        let index = state
            .entry_parent_order
            .iter()
            .position(|id| !state.active_entry_ids.contains(id))
            .unwrap_or(0);
        let Some(evicted) = state.entry_parent_order.remove(index) else {
            break;
        };
        state.entry_parents.remove(&evicted);
        state.entry_turns.remove(&evicted);
    }
}

struct Window {
    records: Vec<NativeRecord>,
    complete_end: u64,
    older_anchor: u64,
    next_tail_boundary: bool,
}

struct OpenSource {
    file: File,
    fingerprint: super::SourceFingerprint,
}

fn open_source(path: &Path) -> Result<OpenSource, TranscriptError> {
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt;
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(path)
    };
    #[cfg(not(unix))]
    let file = OpenOptions::new().read(true).open(path);
    let file = file.map_err(|_| TranscriptError::Unreadable)?;
    let identity_token = crate::platform::conversation_source_identity_for_file(&file)
        .ok_or(TranscriptError::Unreadable)?;
    let (size, modified) = crate::platform::conversation_source_size_modified_for_file(&file)
        .ok_or(TranscriptError::Unreadable)?;
    Ok(OpenSource {
        file,
        fingerprint: super::SourceFingerprint {
            identity_token,
            size,
            modified,
            canonical_path: path.to_path_buf(),
        },
    })
}

fn read_window(
    file: &mut File,
    start: u64,
    end: u64,
    adapter: &dyn ProviderAdapter,
    skip_leading_fragment: bool,
    max_scan_bytes: u64,
    file_size: u64,
    display_root: Option<&Path>,
) -> Result<Window, TranscriptError> {
    if end < start || end - start > max_scan_bytes {
        return Err(TranscriptError::ScanLimitExceeded);
    }
    file.seek(SeekFrom::Start(start))
        .map_err(|_| TranscriptError::Unreadable)?;
    let mut bytes = vec![0; (end - start) as usize];
    file.read_exact(&mut bytes)
        .map_err(|_| TranscriptError::Unreadable)?;

    let mut cursor = 0usize;
    let mut effective_start = start;
    let mid_record_start = if skip_leading_fragment && start > 0 {
        // Only treat the first line as a fragment when start is genuinely
        // inside a record. When the byte before start is a newline, start is
        // a row boundary and the first line is complete and must be parsed
        // (otherwise exact-boundary windows repeatedly drop the same row).
        let mut previous = [0u8; 1];
        file.seek(SeekFrom::Start(start - 1))
            .and_then(|_| file.read_exact(&mut previous))
            .map(|_| previous[0] != b'\n')
            .unwrap_or(false)
    } else {
        false
    };
    file.seek(SeekFrom::Start(start))
        .map_err(|_| TranscriptError::Unreadable)?;
    if mid_record_start {
        if let Some(newline) = bytes.iter().position(|byte| *byte == b'\n') {
            cursor = newline + 1;
            effective_start = start + cursor as u64;
        } else {
            let oversized = bytes.len() >= MAX_RECORD_BYTES;
            return Ok(Window {
                records: Vec::new(),
                complete_end: if oversized { end } else { start },
                older_anchor: start,
                next_tail_boundary: oversized,
            });
        }
    }

    let mut records = Vec::new();
    let mut complete_end = effective_start;
    while cursor < bytes.len() {
        let Some(relative_end) = bytes[cursor..].iter().position(|byte| *byte == b'\n') else {
            break;
        };
        let line_end = cursor + relative_end;
        let line = &bytes[cursor..line_end];
        let line_start = start + cursor as u64;
        complete_end = start + line_end as u64 + 1;
        cursor = line_end + 1;
        if line.is_empty() || line.len() > MAX_RECORD_BYTES {
            continue;
        }
        let Ok(text) = std::str::from_utf8(line) else {
            continue;
        };
        let mut normalized = adapter.normalize_line_for_display(text, display_root);
        for record in &mut normalized {
            record.anchor = line_start;
        }
        records.extend(normalized);
    }
    let trailing_len = bytes.len().saturating_sub(cursor);
    let trailing_oversized = trailing_len >= MAX_RECORD_BYTES;
    if trailing_len > 0 {
        if trailing_oversized {
            complete_end = end;
        } else {
            complete_end = start + cursor as u64;
        }
    }
    if end < file_size && trailing_len == 0 {
        complete_end = end;
    }
    Ok(Window {
        records,
        complete_end,
        older_anchor: effective_start,
        next_tail_boundary: trailing_len == 0 || trailing_oversized,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_conversation::pi::PiAdapter;
    use crate::agent_conversation::{SourceFingerprint, SourceReaderState};
    use std::collections::HashSet;
    use std::time::SystemTime;

    #[cfg(unix)]
    #[test]
    fn source_open_rejects_a_final_symlink() {
        let base = std::env::temp_dir().join(format!("herdr-source-open-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let target = base.join("target.jsonl");
        let link = base.join("link.jsonl");
        std::fs::write(&target, "{}\n").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(super::open_source(&link).is_err());
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn pi_parent_chain_groups_a_turn_and_stabilizes_plan_identity() {
        let mut state = SourceReaderState::new(
            SourceFingerprint {
                identity_token: "source".into(),
                size: 0,
                modified: SystemTime::UNIX_EPOCH,
                canonical_path: std::path::PathBuf::from("/tmp/source.jsonl"),
            },
            crate::agent_conversation::ReaderLimits::production(),
        );
        let records = [
            r#"{"type":"message","id":"u1","parentId":null,"message":{"role":"user","content":"hello"}}"#,
            r#"{"type":"message","id":"a1","parentId":"u1","message":{"role":"assistant","content":[{"type":"toolCall","id":"plan-call","name":"plan","arguments":{"items":[{"content":"Run tests"}]}}]}}"#,
            r#"{"type":"message","id":"r1","parentId":"a1","message":{"role":"toolResult","toolCallId":"plan-call","content":"done"}}"#,
        ]
        .into_iter()
        .flat_map(|line| crate::agent_conversation::pi::PiAdapter.normalize_line(line))
        .collect::<Vec<_>>();
        let visible = ingest_provider_records(
            &mut state,
            &crate::agent_conversation::pi::PiAdapter,
            records,
            true,
        );
        assert!(visible
            .iter()
            .filter(|record| !record.topology_only)
            .all(|record| { record.turn_id.as_deref() == Some("u1") }));
        let plan_ids = visible
            .iter()
            .filter(|record| {
                matches!(
                    record.payload,
                    crate::api::schema::conversations::ConversationItemPayload::PlanUpdate { .. }
                )
            })
            .filter_map(|record| record.native_id.as_deref())
            .collect::<Vec<_>>();
        assert_eq!(plan_ids, ["plan:u1"]);
        assert!(visible
            .iter()
            .filter(|record| !record.topology_only)
            .all(|record| record.turn_id.as_deref() == Some("u1")));
    }

    #[test]
    fn pi_live_turn_identity_matches_durable_parent_ancestry() {
        let mut state = SourceReaderState::new(
            SourceFingerprint {
                identity_token: "source".into(),
                size: 0,
                modified: SystemTime::UNIX_EPOCH,
                canonical_path: std::path::PathBuf::from("/tmp/source.jsonl"),
            },
            crate::agent_conversation::ReaderLimits::production(),
        );
        let rows = [
            r#"{"type":"message","id":"u1","parentId":null,"message":{"role":"user","timestamp":1700000000000,"content":"hello"}}"#,
            r#"{"type":"message","id":"a1","parentId":"u1","message":{"role":"assistant","content":[{"type":"text","text":"done"}],"stopReason":"stop"}}"#,
        ];
        let visible = rows
            .into_iter()
            .flat_map(|line| crate::agent_conversation::pi::PiAdapter.normalize_line(line))
            .collect::<Vec<_>>();
        let visible = ingest_provider_records(
            &mut state,
            &crate::agent_conversation::pi::PiAdapter,
            visible,
            true,
        );
        assert!(visible
            .iter()
            .all(|record| { record.turn_id.as_deref() == Some("turn:1700000000000") }));
        assert!(visible.iter().any(|record| {
            matches!(
                record.payload,
                crate::api::schema::conversations::ConversationItemPayload::AssistantMessage { .. }
            ) && record.native_id.as_deref() == Some("message:turn:1700000000000:0")
        }));
    }

    #[test]
    fn canonical_live_message_reconciles_with_durable_message_without_a_duplicate() {
        let mut state = SourceReaderState::new(
            SourceFingerprint {
                identity_token: "source".into(),
                size: 0,
                modified: SystemTime::UNIX_EPOCH,
                canonical_path: std::path::PathBuf::from("/tmp/source.jsonl"),
            },
            crate::agent_conversation::ReaderLimits::production(),
        );
        let turn_id = "turn:1700000000000";
        crate::agent_conversation::append_overlay(
            &mut state,
            "session",
            &crate::agent_conversation::OverlayRecord {
                native_id: Some(format!("message:{turn_id}:1700000000001")),
                entry_id: None,
                timestamp_ms: Some(1700000000001),
                turn_id: Some(turn_id.into()),
                payload:
                    crate::api::schema::conversations::ConversationItemPayload::AssistantMessage {
                        phase: crate::api::schema::conversations::AssistantMessagePhase::Final,
                        text: "live response".into(),
                        state: crate::api::schema::conversations::CompletionState::Completed,
                    },
            },
        );
        let records = ingest_provider_records(
            &mut state,
            &crate::agent_conversation::pi::PiAdapter,
            vec![NativeRecord {
                native_id: Some("provider-entry-a1".into()),
                entry_id: Some("a1".into()),
                parent_id: Some("u1".into()),
                timestamp_ms: Some(1700000000001),
                turn_id: Some(turn_id.into()),
                payload:
                    crate::api::schema::conversations::ConversationItemPayload::AssistantMessage {
                        phase: crate::api::schema::conversations::AssistantMessagePhase::Final,
                        text: "durable response".into(),
                        state: crate::api::schema::conversations::CompletionState::Completed,
                    },
                topology_only: false,
                anchor: 4,
            }],
            true,
        );
        for record in records {
            let anchor = record.anchor;
            crate::agent_conversation::append_durable(&mut state, "session", &record, anchor);
        }
        assert_eq!(state.log.len(), 1);
        assert!(!state.log[0].overlay);
        assert_eq!(
            state.log[0].native_id.as_deref(),
            Some("message:turn:1700000000000:1700000000001")
        );
    }

    #[test]
    fn descendant_turns_and_plan_identity_are_repaired_when_the_ancestor_arrives_later() {
        let mut state = SourceReaderState::new(
            SourceFingerprint {
                identity_token: "source".into(),
                size: 0,
                modified: SystemTime::UNIX_EPOCH,
                canonical_path: std::path::PathBuf::from("/tmp/source.jsonl"),
            },
            crate::agent_conversation::ReaderLimits::production(),
        );
        let descendants = vec![
            NativeRecord {
                native_id: Some("provider-a1".into()),
                entry_id: Some("a1".into()),
                parent_id: Some("u1".into()),
                timestamp_ms: Some(1700000000100),
                turn_id: None,
                payload:
                    crate::api::schema::conversations::ConversationItemPayload::AssistantMessage {
                        phase: crate::api::schema::conversations::AssistantMessagePhase::Final,
                        text: "done".into(),
                        state: crate::api::schema::conversations::CompletionState::Completed,
                    },
                topology_only: false,
                anchor: 1,
            },
            NativeRecord {
                native_id: Some("plan:todo-1".into()),
                entry_id: Some("a1".into()),
                parent_id: Some("u1".into()),
                timestamp_ms: Some(1700000000100),
                turn_id: None,
                payload: crate::api::schema::conversations::ConversationItemPayload::PlanUpdate {
                    steps: vec![],
                },
                topology_only: false,
                anchor: 1,
            },
        ];
        for record in ingest_provider_records(
            &mut state,
            &crate::agent_conversation::pi::PiAdapter,
            descendants,
            true,
        ) {
            let anchor = record.anchor;
            crate::agent_conversation::append_durable(&mut state, "session", &record, anchor);
        }
        assert!(state.log.iter().all(|entry| entry.turn_id.is_none()));

        let ancestor = NativeRecord {
            native_id: Some("provider-u1".into()),
            entry_id: Some("u1".into()),
            parent_id: None,
            timestamp_ms: Some(1700000000000),
            turn_id: Some("turn:1700000000000".into()),
            payload: crate::api::schema::conversations::ConversationItemPayload::UserMessage {
                text: "hello".into(),
                attachments: vec![],
            },
            topology_only: false,
            anchor: 2,
        };
        let _ = ingest_provider_records(
            &mut state,
            &crate::agent_conversation::pi::PiAdapter,
            vec![ancestor],
            false,
        );
        let assistant = state
            .log
            .iter()
            .find(|entry| matches!(entry.payload, crate::api::schema::conversations::ConversationItemPayload::AssistantMessage { .. }))
            .unwrap();
        assert_eq!(assistant.turn_id.as_deref(), Some("turn:1700000000000"));
        assert_eq!(
            assistant.native_id.as_deref(),
            Some("message:turn:1700000000000:1700000000100")
        );
        let plans = state
            .log
            .iter()
            .filter(|entry| {
                matches!(
                    entry.payload,
                    crate::api::schema::conversations::ConversationItemPayload::PlanUpdate { .. }
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(plans.len(), 1);
        assert_eq!(
            plans[0].native_id.as_deref(),
            Some("plan:turn:1700000000000")
        );
    }

    #[test]
    fn omp_plan_projection_uses_the_parent_derived_turn_identity() {
        let mut state = SourceReaderState::new(
            SourceFingerprint {
                identity_token: "source".into(),
                size: 0,
                modified: SystemTime::UNIX_EPOCH,
                canonical_path: std::path::PathBuf::from("/tmp/source.jsonl"),
            },
            crate::agent_conversation::ReaderLimits::production(),
        );
        let rows = [
            r#"{"type":"message","id":"u1","parentId":null,"message":{"role":"user","content":"hello"}}"#,
            r#"{"type":"message","id":"p1","parentId":"u1","message":{"role":"assistant","content":[{"type":"toolCall","id":"todo-1","name":"todo","arguments":{"list":[{"content":"Run tests","status":"pending"}]}}]}}"#,
            r#"{"type":"message","id":"r1","parentId":"p1","message":{"role":"toolResult","toolCallId":"todo-1","details":{"phases":[{"name":"Checks","tasks":[{"content":"Run tests","status":"completed"}]}]},"isError":false}}"#,
        ];
        let records = rows
            .into_iter()
            .flat_map(|line| crate::agent_conversation::omp::OmpAdapter.normalize_line(line))
            .collect::<Vec<_>>();
        let visible = ingest_provider_records(
            &mut state,
            &crate::agent_conversation::omp::OmpAdapter,
            records,
            true,
        );
        let plans = visible
            .iter()
            .filter(|record| {
                matches!(
                    record.payload,
                    crate::api::schema::conversations::ConversationItemPayload::PlanUpdate { .. }
                )
            })
            .collect::<Vec<_>>();
        assert!(!plans.is_empty());
        assert!(plans
            .iter()
            .all(|record| record.turn_id.as_deref() == Some("u1")));
        assert!(plans
            .iter()
            .all(|record| record.native_id.as_deref() == Some("plan:u1")));
    }

    #[test]
    fn codex_lifecycle_turns_group_rows_without_inline_metadata() {
        let mut state = SourceReaderState::new(
            SourceFingerprint {
                identity_token: "source".into(),
                size: 0,
                modified: SystemTime::UNIX_EPOCH,
                canonical_path: std::path::PathBuf::from("/tmp/source.jsonl"),
            },
            crate::agent_conversation::ReaderLimits::production(),
        );
        let records = [
            r#"{"type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hello"}]}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}]}}"#,
        ]
        .into_iter()
        .flat_map(|line| crate::agent_conversation::codex::CodexAdapter.normalize_line(line))
        .collect::<Vec<_>>();
        let visible = ingest_provider_records(
            &mut state,
            &crate::agent_conversation::codex::CodexAdapter,
            records,
            true,
        );
        assert!(visible
            .iter()
            .filter(|record| !record.topology_only)
            .all(|record| { record.turn_id.as_deref() == Some("turn-1") }));
    }

    #[test]
    fn claude_parent_chain_groups_a_turn_without_explicit_turn_ids() {
        let mut state = SourceReaderState::new(
            SourceFingerprint {
                identity_token: "source".into(),
                size: 0,
                modified: SystemTime::UNIX_EPOCH,
                canonical_path: std::path::PathBuf::from("/tmp/source.jsonl"),
            },
            crate::agent_conversation::ReaderLimits::production(),
        );
        let records = [
            r#"{"type":"user","uuid":"u1","parentUuid":null,"message":{"role":"user","content":"hello"}}"#,
            r#"{"type":"assistant","uuid":"a1","parentUuid":"u1","message":{"role":"assistant","content":[{"type":"text","text":"working"}]}}"#,
            r#"{"type":"user","uuid":"r1","parentUuid":"a1","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tool-1","content":"done"}]}}"#,
            r#"{"type":"assistant","uuid":"a2","parentUuid":"r1","message":{"role":"assistant","content":[{"type":"text","text":"done"}]}}"#,
        ]
        .into_iter()
        .flat_map(|line| crate::agent_conversation::claude::ClaudeAdapter.normalize_line(line))
        .collect::<Vec<_>>();
        let visible = ingest_provider_records(
            &mut state,
            &crate::agent_conversation::claude::ClaudeAdapter,
            records,
            true,
        );
        assert!(
            visible
                .iter()
                .filter(|record| !record.topology_only)
                .all(|record| record.turn_id.as_deref() == Some("u1")),
            "turns={:?}",
            visible
                .iter()
                .map(|record| (&record.entry_id, &record.turn_id))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn claude_session_identity_does_not_merge_distinct_turns() {
        let mut state = SourceReaderState::new(
            SourceFingerprint {
                identity_token: "source".into(),
                size: 0,
                modified: SystemTime::UNIX_EPOCH,
                canonical_path: std::path::PathBuf::from("/tmp/source.jsonl"),
            },
            crate::agent_conversation::ReaderLimits::production(),
        );
        let records = [
            r#"{"type":"user","uuid":"u1","parentUuid":null,"sessionId":"session-1","message":{"role":"user","content":"first"}}"#,
            r#"{"type":"assistant","uuid":"a1","parentUuid":"u1","sessionId":"session-1","message":{"role":"assistant","content":[{"type":"text","text":"first answer"}],"stop_reason":"end_turn"}}"#,
            r#"{"type":"user","uuid":"u2","parentUuid":"a1","sessionId":"session-1","message":{"role":"user","content":"second"}}"#,
            r#"{"type":"assistant","uuid":"a2","parentUuid":"u2","sessionId":"session-1","message":{"role":"assistant","content":[{"type":"text","text":"second answer"}],"stop_reason":"end_turn"}}"#,
        ]
        .into_iter()
        .flat_map(|line| crate::agent_conversation::claude::ClaudeAdapter.normalize_line(line))
        .collect::<Vec<_>>();
        let visible = ingest_provider_records(
            &mut state,
            &crate::agent_conversation::claude::ClaudeAdapter,
            records,
            true,
        );
        let turns = visible
            .iter()
            .filter(|record| !record.topology_only)
            .map(|record| record.turn_id.as_deref())
            .collect::<Vec<_>>();

        assert_eq!(turns, vec![Some("u1"), Some("u1"), Some("u2"), Some("u2")]);
    }

    #[test]
    fn claude_visible_rows_stay_with_the_active_turn_across_hidden_parent_rows() {
        let mut state = SourceReaderState::new(
            SourceFingerprint {
                identity_token: "source".into(),
                size: 0,
                modified: SystemTime::UNIX_EPOCH,
                canonical_path: std::path::PathBuf::from("/tmp/source.jsonl"),
            },
            crate::agent_conversation::ReaderLimits::production(),
        );
        let records = [
            r#"{"type":"user","uuid":"u1","parentUuid":null,"sessionId":"session-1","message":{"role":"user","content":"first"}}"#,
            r#"{"type":"assistant","uuid":"a1","parentUuid":"hidden-attachment","sessionId":"session-1","message":{"role":"assistant","content":[{"type":"text","text":"first answer"}],"stop_reason":"end_turn"}}"#,
            r#"{"type":"user","uuid":"u2","parentUuid":"hidden-turn-state","sessionId":"session-1","message":{"role":"user","content":"second"}}"#,
            r#"{"type":"assistant","uuid":"a2","parentUuid":"hidden-attachment-2","sessionId":"session-1","message":{"role":"assistant","content":[{"type":"text","text":"second answer"}],"stop_reason":"end_turn"}}"#,
        ]
        .into_iter()
        .flat_map(|line| crate::agent_conversation::claude::ClaudeAdapter.normalize_line(line))
        .collect::<Vec<_>>();
        let visible = ingest_provider_records(
            &mut state,
            &crate::agent_conversation::claude::ClaudeAdapter,
            records,
            true,
        );
        let turns = visible
            .iter()
            .map(|record| record.turn_id.as_deref())
            .collect::<Vec<_>>();

        assert_eq!(turns, vec![Some("u1"), Some("u1"), Some("u2"), Some("u2")]);
    }

    #[test]
    fn truncated_branch_topology_does_not_hide_unknown_active_ancestors() {
        let mut state = SourceReaderState::new(
            SourceFingerprint {
                identity_token: "source".into(),
                size: 0,
                modified: SystemTime::UNIX_EPOCH,
                canonical_path: std::path::PathBuf::from("/tmp/source.jsonl"),
            },
            crate::agent_conversation::ReaderLimits::test_small(),
        );
        let rows = [
            r#"{"type":"message","id":"root","parentId":null,"message":{"role":"user","content":"root"}}"#,
            r#"{"type":"message","id":"a1","parentId":"root","message":{"role":"user","content":"a1"}}"#,
            r#"{"type":"message","id":"b1","parentId":"root","message":{"role":"user","content":"abandoned"}}"#,
            r#"{"type":"message","id":"a2","parentId":"a1","message":{"role":"user","content":"a2"}}"#,
            r#"{"type":"message","id":"a3","parentId":"a2","message":{"role":"user","content":"a3"}}"#,
            r#"{"type":"message","id":"a4","parentId":"a3","message":{"role":"user","content":"a4"}}"#,
        ];
        let records = rows
            .into_iter()
            .flat_map(|line| crate::agent_conversation::pi::PiAdapter.normalize_line(line))
            .collect::<Vec<_>>();
        ingest_provider_records(
            &mut state,
            &crate::agent_conversation::pi::PiAdapter,
            records,
            true,
        );
        let older = [
            r#"{"type":"message","id":"root","parentId":null,"message":{"role":"user","content":"root"}}"#,
            r#"{"type":"message","id":"a1","parentId":"root","message":{"role":"user","content":"a1"}}"#,
        ]
        .into_iter()
        .flat_map(|line| crate::agent_conversation::pi::PiAdapter.normalize_line(line))
        .collect::<Vec<_>>();
        let visible = ingest_provider_records(
            &mut state,
            &crate::agent_conversation::pi::PiAdapter,
            older,
            false,
        );
        assert!(visible
            .iter()
            .any(|record| record.entry_id.as_deref() == Some("root")));
        assert!(visible
            .iter()
            .any(|record| record.entry_id.as_deref() == Some("a1")));
    }

    #[test]
    fn topology_and_active_membership_stay_bounded_across_linear_ingestion() {
        let mut state = SourceReaderState::new(
            SourceFingerprint {
                identity_token: "source".into(),
                size: 0,
                modified: SystemTime::UNIX_EPOCH,
                canonical_path: std::path::PathBuf::from("/tmp/source.jsonl"),
            },
            crate::agent_conversation::ReaderLimits::test_small(),
        );
        let mut parent = "null".to_string();
        for index in 0..32 {
            let line = format!(
                r#"{{"type":"message","id":"row-{index}","parentId":{parent},"message":{{"role":"user","content":"row-{index}"}}}}"#
            );
            parent = format!("\"row-{index}\"");
            let records = crate::agent_conversation::pi::PiAdapter.normalize_line(&line);
            ingest_provider_records(
                &mut state,
                &crate::agent_conversation::pi::PiAdapter,
                records,
                true,
            );
        }
        assert!(state.topology.len() <= 4);
        assert!(state.active_entry_ids.len() <= 4);
    }

    #[test]
    fn older_pages_exclude_records_from_abandoned_branches() {
        let mut state = SourceReaderState::new(
            SourceFingerprint {
                identity_token: "source".into(),
                size: 0,
                modified: SystemTime::UNIX_EPOCH,
                canonical_path: std::path::PathBuf::from("/tmp/source.jsonl"),
            },
            crate::agent_conversation::ReaderLimits::production(),
        );
        state.active_entry_ids = HashSet::from(["root".to_string(), "active".to_string()]);
        state.active_branch_complete = true;
        state.topology_branching = true;
        state.entry_parents.insert("root".into(), None);
        state.entry_parent_order.push_back("root".into());
        let records = [
            r#"{"type":"message","id":"abandoned","parentId":"root","message":{"role":"assistant","content":[{"type":"text","text":"old"}],"stopReason":"stop"}}"#,
            r#"{"type":"message","id":"active","parentId":"root","message":{"role":"assistant","content":[{"type":"text","text":"new"}],"stopReason":"stop"}}"#,
        ]
        .into_iter()
        .flat_map(|line| PiAdapter.normalize_line(line))
        .collect::<Vec<_>>();

        let filtered = ingest_provider_records(&mut state, &PiAdapter, records, false);
        assert!(filtered.iter().all(|record| {
            record
                .entry_id
                .as_deref()
                .is_some_and(|entry_id| entry_id == "active")
        }));
    }
}
