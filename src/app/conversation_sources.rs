use std::collections::HashMap;
use std::path::PathBuf;

use crate::agent_resume::TranscriptRef;
use crate::layout::PaneId;

/// App-level conversation-source registry.
///
/// Keeps the engine-side transcript source and its random opaque
/// conversation handle strictly separate from the large TerminalState
/// session-authority machine. Entries are written only after the existing
/// session setter accepts a report, and are ignored whenever the stored
/// session identity no longer matches the terminal's current accepted
/// session (the next accepted report or clear replaces them). Transcript
/// paths and session values never leave this module publicly.
#[derive(Debug, Clone, Default)]
pub struct ConversationSourceRegistry {
    entries: HashMap<PaneId, ConversationSourceEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationSourceEntry {
    /// Session identity this entry belongs to (source, agent, resume
    /// kind/value) at acceptance time.
    session_identity: String,
    /// Engine-side transcript source; never serialized publicly.
    transcript_ref: Option<TranscriptRef>,
    capability: crate::api::schema::ConversationCapability,
    /// Random opaque conversation-session handle; stable for the accepted
    /// engine session, rotated on session replacement and changed across
    /// engine restarts (which forces a reader-generation reset).
    pub(crate) conversation_handle: Option<String>,
}

impl ConversationSourceRegistry {
    pub(crate) fn restore_from_snapshot(
        snapshot: &crate::persist::SessionSnapshot,
        workspaces: &[crate::workspace::Workspace],
    ) -> Self {
        let aliases = crate::persist::handoff_pane_aliases(snapshot, workspaces);
        let mut registry = Self::default();
        for (workspace_snapshot, workspace) in snapshot.workspaces.iter().zip(workspaces) {
            for (tab_snapshot, tab) in workspace_snapshot.tabs.iter().zip(&workspace.tabs) {
                for (old_id, pane_snapshot) in &tab_snapshot.panes {
                    let Some(session) = pane_snapshot.agent_session.as_ref() else {
                        continue;
                    };
                    let Some(path) = restored_transcript_path(
                        pane_snapshot.transcript_path.clone(),
                        Some(session),
                    ) else {
                        continue;
                    };
                    let pane_id = aliases
                        .get(old_id)
                        .copied()
                        .unwrap_or_else(|| PaneId::from_raw(*old_id));
                    if !tab.panes.contains_key(&pane_id) {
                        continue;
                    }
                    let Some(transcript_ref) = TranscriptRef::new(&session.agent, path) else {
                        continue;
                    };
                    registry.accept(
                        pane_id,
                        session_identity_key(
                            &session.source,
                            &session.agent,
                            session.kind.kind_name(),
                            &session.value,
                        ),
                        &session.agent,
                        Some(transcript_ref),
                    );
                }
            }
        }
        registry
    }

    pub fn accept(
        &mut self,
        pane_id: PaneId,
        session_identity: String,
        agent: &str,
        transcript_ref: Option<TranscriptRef>,
    ) {
        let capability = source_capability(agent, transcript_ref.as_ref());
        if let Some(entry) = self.entries.get_mut(&pane_id).filter(|entry| {
            entry.session_identity == session_identity && entry.transcript_ref == transcript_ref
        }) {
            entry.capability = capability;
            return;
        }
        let conversation_handle = crate::agent_resume::generate_conversation_handle();
        self.entries.insert(
            pane_id,
            ConversationSourceEntry {
                session_identity,
                transcript_ref,
                capability,
                conversation_handle,
            },
        );
    }

    pub fn clear(&mut self, pane_id: PaneId) {
        self.entries.remove(&pane_id);
    }

    pub(crate) fn transcript_path_for_persistence(&self, pane_id: PaneId) -> Option<PathBuf> {
        self.entries
            .get(&pane_id)
            .and_then(|entry| entry.transcript_ref.as_ref())
            .map(|transcript| transcript.path.clone())
    }

    /// Returns the entry only when its stored session identity matches the
    /// terminal's current accepted session; stale entries are ignored.
    pub fn current_for(
        &self,
        pane_id: PaneId,
        current_session_identity: Option<&str>,
    ) -> Option<ConversationSourceEntry> {
        let entry = self.entries.get(&pane_id)?;
        match current_session_identity {
            Some(identity) if identity == entry.session_identity => Some(entry.clone()),
            _ => None,
        }
    }

    #[cfg(test)]
    pub fn transcript_for(
        &self,
        pane_id: PaneId,
        current_session_identity: Option<&str>,
    ) -> Option<TranscriptRef> {
        self.current_for(pane_id, current_session_identity)
            .and_then(|entry| entry.transcript_ref)
    }
}

fn restored_transcript_path(
    transcript_path: Option<PathBuf>,
    session: Option<&crate::persist::PaneAgentSessionSnapshot>,
) -> Option<PathBuf> {
    transcript_path.or_else(|| {
        session.and_then(|session| {
            (session.kind == crate::agent_resume::AgentSessionRefKind::Path)
                .then(|| PathBuf::from(&session.value))
        })
    })
}

impl ConversationSourceEntry {
    pub(crate) fn transcript_ref(&self) -> Option<&TranscriptRef> {
        self.transcript_ref.as_ref()
    }

    pub(crate) fn conversation_handle(&self) -> Option<&str> {
        self.conversation_handle.as_deref()
    }

    pub(crate) fn capability(&self) -> &crate::api::schema::ConversationCapability {
        &self.capability
    }
}

fn source_capability(
    agent: &str,
    transcript_ref: Option<&TranscriptRef>,
) -> crate::api::schema::ConversationCapability {
    use crate::agent_conversation::TranscriptError;
    use crate::api::schema::{
        ConversationAvailability as Availability, ConversationCapability as Capability,
        ConversationReasonCode as Reason,
    };

    match transcript_ref {
        Some(transcript) => {
            match crate::agent_conversation::validate_transcript_source(agent, &transcript.path) {
                Ok(_) => Capability {
                    availability: Availability::Supported,
                    reason: Reason::Ready,
                    message: None,
                },
                Err(TranscriptError::Missing) => Capability {
                    availability: Availability::Unavailable,
                    reason: Reason::TranscriptMissing,
                    message: Some("The agent transcript is not available yet.".into()),
                },
                Err(TranscriptError::Invalid) => Capability {
                    availability: Availability::Unavailable,
                    reason: Reason::TranscriptInvalid,
                    message: Some("The agent transcript is invalid.".into()),
                },
                Err(_) => Capability {
                    availability: Availability::Unavailable,
                    reason: Reason::SourceUnreadable,
                    message: Some("The agent transcript is temporarily unreadable.".into()),
                },
            }
        }
        None => Capability {
            availability: Availability::Unavailable,
            reason: Reason::TranscriptMissing,
            message: Some("Start or resume this agent to use Chat.".into()),
        },
    }
}

/// Deterministic session identity key derived from an accepted report or the
/// terminal's current session. Internal only; never serialized.
pub fn session_identity_key(source: &str, agent: &str, kind: &str, value: &str) -> String {
    format!("{source}:{agent}:{kind}:{value}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_resume::TranscriptRef;

    #[test]
    fn entry_is_ignored_when_session_identity_changes() {
        let mut registry = ConversationSourceRegistry::default();
        let pane = PaneId::from_raw(1);

        registry.accept(pane, "key-1".into(), "pi", None);
        assert!(registry.current_for(pane, Some("key-1")).is_some());
        // A changed accepted session invalidates the stored source.
        assert!(registry.current_for(pane, Some("key-2")).is_none());
        assert!(registry.transcript_for(pane, Some("key-2")).is_none());
        // No session at all also invalidates.
        assert!(registry.current_for(pane, None).is_none());
    }

    #[test]
    fn duplicate_accepted_report_keeps_conversation_handle() {
        let mut registry = ConversationSourceRegistry::default();
        let pane = PaneId::from_raw(3);
        let transcript = TranscriptRef::new("codex", "/tmp/codex.jsonl").unwrap();
        registry.accept(pane, "key-1".into(), "codex", Some(transcript.clone()));
        let first = registry
            .current_for(pane, Some("key-1"))
            .unwrap()
            .conversation_handle
            .clone();
        registry.accept(pane, "key-1".into(), "codex", Some(transcript));
        let second = registry
            .current_for(pane, Some("key-1"))
            .unwrap()
            .conversation_handle
            .clone();
        assert_eq!(first, second);
    }

    #[test]
    fn accept_replaces_and_clear_removes() {
        let mut registry = ConversationSourceRegistry::default();
        let pane = PaneId::from_raw(2);
        let transcript = TranscriptRef::new("codex", "/tmp/codex.jsonl").unwrap();

        registry.accept(pane, "key-1".into(), "codex", Some(transcript.clone()));
        let entry = registry.current_for(pane, Some("key-1")).unwrap();
        assert_eq!(entry.transcript_ref, Some(transcript));
        assert!(entry.conversation_handle.is_some());

        registry.accept(pane, "key-2".into(), "codex", None);
        assert!(registry.transcript_for(pane, Some("key-2")).is_none());

        registry.clear(pane);
        assert!(registry.current_for(pane, Some("key-2")).is_none());
    }

    #[test]
    fn session_identity_key_matches_across_accept_and_read() {
        let key = session_identity_key("herdr:pi", "pi", "path", "/tmp/pi.jsonl");
        assert_eq!(key, "herdr:pi:pi:path:/tmp/pi.jsonl");
        let id_key = session_identity_key("herdr:claude", "claude", "id", "abc");
        assert_eq!(id_key, "herdr:claude:claude:id:abc");
        assert_ne!(key, id_key);
    }

    #[test]
    fn pane_capability_reads_use_the_validation_cached_at_acceptance() {
        let _guard = crate::config::test_config_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = std::env::temp_dir().join(format!(
            "herdr-conversation-capability-{}",
            std::process::id()
        ));
        let transcript_path = root.join("sessions").join("conversation.jsonl");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(transcript_path.parent().unwrap()).unwrap();
        std::fs::write(&transcript_path, b"{}\n").unwrap();
        let previous = std::env::var_os("PI_CODING_AGENT_DIR");
        std::env::set_var("PI_CODING_AGENT_DIR", &root);

        let mut registry = ConversationSourceRegistry::default();
        let pane = PaneId::from_raw(4);
        registry.accept(
            pane,
            "key-1".into(),
            "pi",
            TranscriptRef::new("pi", &transcript_path),
        );
        std::fs::remove_file(&transcript_path).unwrap();

        let entry = registry.current_for(pane, Some("key-1")).unwrap();
        assert_eq!(
            entry.capability().availability,
            crate::api::schema::ConversationAvailability::Supported
        );

        match previous {
            Some(value) => std::env::set_var("PI_CODING_AGENT_DIR", value),
            None => std::env::remove_var("PI_CODING_AGENT_DIR"),
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn legacy_path_session_restores_its_transcript_source() {
        let session = crate::persist::PaneAgentSessionSnapshot {
            source: "herdr:pi".into(),
            agent: "pi".into(),
            kind: crate::agent_resume::AgentSessionRefKind::Path,
            value: "/tmp/pi-session.jsonl".into(),
        };

        assert_eq!(
            restored_transcript_path(None, Some(&session)),
            Some(PathBuf::from("/tmp/pi-session.jsonl"))
        );
    }
}
