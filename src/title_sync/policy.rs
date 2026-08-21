use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AgentSession {
    pub(crate) agent: Option<String>,
    pub(crate) kind: Option<String>,
    pub(crate) value: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PaneInput {
    pub(crate) pane_id: String,
    pub(crate) agent: Option<String>,
    pub(crate) agent_session: Option<AgentSession>,
    pub(crate) cwd: Option<PathBuf>,
    pub(crate) foreground_cwd: Option<PathBuf>,
    pub(crate) label: Option<String>,
    pub(crate) terminal_title: Option<String>,
    pub(crate) terminal_title_stripped: Option<String>,
    pub(crate) shell_pid: Option<u32>,
    pub(crate) foreground_processes: Vec<ProcessInput>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ProcessInput {
    pub(crate) name: String,
    pub(crate) argv: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct Ownership {
    pub(crate) session: Option<String>,
    pub(crate) title: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RenameDecision {
    Clear,
    Manual,
    Noop,
    Rename,
}

pub(crate) fn has_agent_identity(pane: &PaneInput) -> bool {
    agent_identity(pane).is_some()
}

pub(crate) fn agent_identity(pane: &PaneInput) -> Option<&str> {
    pane.agent.as_deref().or_else(|| {
        pane.agent_session
            .as_ref()
            .and_then(|session| session.agent.as_deref())
    })
}

pub(crate) fn session_key(pane: &PaneInput) -> Option<String> {
    let session = pane.agent_session.as_ref()?;
    let value = session.value.as_deref()?;
    let agent = pane.agent.as_deref().or(session.agent.as_deref())?;
    Some(format!(
        "{agent}:{}:{value}",
        session.kind.as_deref().unwrap_or_default()
    ))
}

pub(crate) fn rename_decision(
    pane: &PaneInput,
    desired_title: Option<&str>,
    previous: Option<&Ownership>,
) -> RenameDecision {
    let current = sanitize_title(pane.label.as_deref(), 200);
    if current.is_some() && previous.map(|state| state.title.as_str()) != current.as_deref() {
        return RenameDecision::Manual;
    }
    if desired_title.is_none()
        && current.is_some()
        && previous.map(|state| state.title.as_str()) == current.as_deref()
        && previous.and_then(|state| state.session.as_deref()) != session_key(pane).as_deref()
    {
        return RenameDecision::Clear;
    }
    if desired_title.is_none() || desired_title == current.as_deref() {
        return RenameDecision::Noop;
    }
    RenameDecision::Rename
}

pub(crate) fn sanitize_title(raw: Option<&str>, max_bytes: usize) -> Option<String> {
    let value = raw?
        .chars()
        .map(|character| {
            if character <= '\u{001f}' || character == '\u{007f}' {
                ' '
            } else {
                character
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if value.is_empty() {
        return None;
    }
    let mut end = value.len().min(max_bytes);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    let value = value[..end].trim_end();
    (!value.is_empty()).then(|| value.to_string())
}

pub(crate) fn read_ownership(root: &Path, pane_id: &str) -> Option<Ownership> {
    let bytes = fs::read(ownership_path(root, pane_id)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub(crate) fn write_ownership(root: &Path, pane_id: &str, state: &Ownership) -> io::Result<()> {
    fs::create_dir_all(root)?;
    let path = ownership_path(root, pane_id);
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    let mut bytes = serde_json::to_vec(state).map_err(io::Error::other)?;
    bytes.push(b'\n');
    fs::write(&temporary, bytes)?;
    if let Err(error) = fs::rename(&temporary, &path) {
        let _ = fs::remove_file(temporary);
        return Err(error);
    }
    Ok(())
}

pub(crate) fn clear_ownership(root: &Path, pane_id: &str) -> io::Result<()> {
    match fs::remove_file(ownership_path(root, pane_id)) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

fn ownership_path(root: &Path, pane_id: &str) -> PathBuf {
    let mut encoded = String::new();
    for byte in pane_id.bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')'
            )
        {
            encoded.push(byte as char);
        } else {
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    root.join(format!("{encoded}.json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn pane(agent: Option<&str>, session_id: Option<&str>, label: Option<&str>) -> PaneInput {
        PaneInput {
            pane_id: "w1:p1".into(),
            agent: agent.map(str::to_string),
            agent_session: session_id.map(|value| AgentSession {
                agent: agent.map(str::to_string),
                kind: Some("id".into()),
                value: Some(value.into()),
            }),
            label: label.map(str::to_string),
            ..PaneInput::default()
        }
    }

    #[test]
    fn title_sanitization_matches_the_pinned_typescript_cases() {
        assert_eq!(
            sanitize_title(Some("  Fix\n  CI  "), 200).as_deref(),
            Some("Fix CI")
        );
        let truncated = sanitize_title(Some(&"𝕏".repeat(60)), 200).expect("title");
        assert!(truncated.len() <= 200);
        assert!(truncated.is_char_boundary(truncated.len()));
    }

    #[test]
    fn rename_policy_matches_the_pinned_typescript_table() {
        let cases = [
            (
                "manual labels win",
                pane(Some("codex"), Some("session-1"), Some("manual")),
                Some("Generated"),
                None,
                RenameDecision::Manual,
            ),
            (
                "owned labels update",
                pane(Some("codex"), Some("session-1"), Some("Old")),
                Some("Generated"),
                Some(Ownership {
                    session: Some("codex:id:session-1".into()),
                    title: "Old".into(),
                }),
                RenameDecision::Rename,
            ),
            (
                "stale sessions clear",
                pane(Some("codex"), Some("session-2"), Some("Old")),
                None,
                Some(Ownership {
                    session: Some("codex:id:session-1".into()),
                    title: "Old".into(),
                }),
                RenameDecision::Clear,
            ),
            (
                "agent_session alone is sufficient",
                PaneInput {
                    pane_id: "w1:p1".into(),
                    agent_session: Some(AgentSession {
                        agent: Some("claude".into()),
                        kind: Some("id".into()),
                        value: Some("session-1".into()),
                    }),
                    ..PaneInput::default()
                },
                Some("Agent task"),
                None,
                RenameDecision::Rename,
            ),
        ];
        for (name, pane, desired, previous, expected) in cases {
            assert_eq!(
                rename_decision(&pane, desired, previous.as_ref()),
                expected,
                "{name}"
            );
        }
    }

    #[test]
    fn ownership_round_trips_per_pane_and_clears() {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "vimeflow-title-policy-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let state = Ownership {
            session: Some("claude:id:session-1".into()),
            title: "Agent task".into(),
        };
        write_ownership(&root, "w1:p1", &state).expect("write ownership");
        assert_eq!(read_ownership(&root, "w1:p1"), Some(state));
        assert!(root.join("w1%3Ap1.json").is_file());
        clear_ownership(&root, "w1:p1").expect("clear ownership");
        assert_eq!(read_ownership(&root, "w1:p1"), None);
        let _ = fs::remove_dir(root);
    }
}
