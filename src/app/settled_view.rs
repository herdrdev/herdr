//! Read-only transcript projection for parked agent sessions.

use std::path::{Path, PathBuf};

const MAX_TRANSCRIPT_BYTES: u64 = 8 * 1024 * 1024;
const MAX_SESSION_SEARCH_ENTRIES: usize = 4_096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SettledTranscript {
    pub source: String,
    pub turns: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SettledViewState {
    pub pane_id: crate::layout::PaneId,
    pub transcript: Option<SettledTranscript>,
    pub command: String,
    pub scroll: usize,
    pub search: String,
    pub searching: bool,
    pub editing: bool,
}

pub(crate) fn load_transcript(
    session: &crate::agent_resume::PersistedAgentSession,
    cwd: &Path,
) -> Option<SettledTranscript> {
    load_transcript_limited(session, cwd, MAX_TRANSCRIPT_BYTES)
        .map(|(transcript, _)| transcript)
        .filter(|transcript| !transcript.turns.is_empty())
}

pub(crate) fn load_transcript_limited(
    session: &crate::agent_resume::PersistedAgentSession,
    cwd: &Path,
    max_bytes: u64,
) -> Option<(SettledTranscript, u64)> {
    let path = session_path(session, cwd)?;
    let metadata = std::fs::metadata(&path).ok()?;
    let file = std::fs::File::open(&path).ok()?;
    use std::io::{Read, Seek, SeekFrom};
    let mut file = file;
    let byte_limit = max_bytes.min(MAX_TRANSCRIPT_BYTES);
    if byte_limit == 0 {
        return None;
    }
    let byte_count = metadata.len().min(byte_limit);
    let start = metadata.len().saturating_sub(byte_count);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.take(byte_count).read_to_end(&mut bytes).ok()?;
    let bytes_read = bytes.len() as u64;
    let text = String::from_utf8_lossy(&bytes);
    let turns = if session.agent.to_ascii_lowercase().contains("claude") {
        parse_claude(&text)
    } else {
        parse_codex(&text)
    };
    Some((
        SettledTranscript {
            source: path.display().to_string(),
            turns,
        },
        bytes_read,
    ))
}

fn session_path(
    session: &crate::agent_resume::PersistedAgentSession,
    cwd: &Path,
) -> Option<PathBuf> {
    use crate::agent_resume::AgentSessionRefKind;
    if session.session_ref.kind == AgentSessionRefKind::Path {
        let path = PathBuf::from(&session.session_ref.value);
        return path.is_file().then_some(path);
    }
    let id = &session.session_ref.value;
    let agent = session.agent.to_ascii_lowercase();
    if agent.contains("claude") {
        let root = std::env::var_os("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".claude")))?;
        let project = cwd
            .to_string_lossy()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect::<String>();
        let direct = root
            .join("projects")
            .join(project)
            .join(format!("{id}.jsonl"));
        if direct.is_file() {
            return Some(direct);
        }
        let mut entries_seen = 0;
        return find_named_file(
            &root.join("projects"),
            &format!("{id}.jsonl"),
            2,
            &mut entries_seen,
        );
    }
    let root = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))?
        .join("sessions");
    find_codex_file(&root, id)
}

fn find_named_file(
    root: &Path,
    name: &str,
    max_depth: usize,
    entries_seen: &mut usize,
) -> Option<PathBuf> {
    let remaining = MAX_SESSION_SEARCH_ENTRIES.saturating_sub(*entries_seen);
    for entry in std::fs::read_dir(root).ok()?.take(remaining).flatten() {
        *entries_seen += 1;
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() && max_depth > 0 {
            if let Some(found) = find_named_file(&path, name, max_depth - 1, entries_seen) {
                return Some(found);
            }
        } else if file_type.is_file() && path.file_name().is_some_and(|file| file == name) {
            return Some(path);
        }
    }
    None
}

fn find_codex_file(root: &Path, id: &str) -> Option<PathBuf> {
    let mut entries_seen = 0usize;
    for year in std::fs::read_dir(root)
        .ok()?
        .take(MAX_SESSION_SEARCH_ENTRIES)
        .flatten()
    {
        entries_seen += 1;
        let year_path = year.path();
        if !year_path.is_dir() {
            continue;
        }
        for month in std::fs::read_dir(&year_path)
            .ok()?
            .take(MAX_SESSION_SEARCH_ENTRIES.saturating_sub(entries_seen))
            .flatten()
        {
            entries_seen += 1;
            let month_path = month.path();
            if !month_path.is_dir() {
                continue;
            }
            for day in std::fs::read_dir(month_path)
                .ok()?
                .take(MAX_SESSION_SEARCH_ENTRIES.saturating_sub(entries_seen))
                .flatten()
            {
                entries_seen += 1;
                let path = day.path();
                let name = path.file_name()?.to_string_lossy();
                if name.starts_with("rollout-") && name.ends_with(&format!("-{id}.jsonl")) {
                    return Some(path);
                }
            }
        }
    }
    None
}

fn parse_claude(text: &str) -> Vec<(String, String)> {
    let mut turns = Vec::new();
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let role = value.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if !matches!(role, "user" | "assistant")
            || value
                .get("isMeta")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            || value
                .get("isSidechain")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        {
            continue;
        }
        let Some(content) = value.pointer("/message/content") else {
            continue;
        };
        let body = match content {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Array(items) => items
                .iter()
                .filter(|item| item.get("type").and_then(|v| v.as_str()) == Some("text"))
                .map(|item| {
                    item.get("text")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string()
                })
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
        if !body.trim().is_empty() {
            turns.push((if role == "user" { "you" } else { "claude" }.into(), body));
        }
    }
    turns
}

fn parse_codex(text: &str) -> Vec<(String, String)> {
    let mut turns = Vec::new();
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let payload = value.pointer("/response_item/payload").unwrap_or(&value);
        if payload.get("type").and_then(|v| v.as_str()) != Some("message") {
            continue;
        }
        let role = payload.get("role").and_then(|v| v.as_str()).unwrap_or("");
        if !matches!(role, "user" | "assistant") {
            continue;
        }
        let body = match payload.get("content") {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(serde_json::Value::Array(items)) => items
                .iter()
                .filter(|item| {
                    matches!(
                        item.get("type").and_then(|v| v.as_str()),
                        Some("input_text" | "output_text" | "text")
                    )
                })
                .map(|item| {
                    item.get("text")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string()
                })
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
        if !body.contains("<environment_context>") && !body.trim().is_empty() {
            turns.push((if role == "user" { "you" } else { "codex" }.into(), body));
        }
    }
    turns
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_visible_claude_and_codex_turns() {
        let claude = concat!(
            r#"{"type":"user","message":{"content":[{"type":"text","text":"hello"}]}}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hi"}]},"isSidechain":true}"#
        );
        assert_eq!(parse_claude(claude), vec![("you".into(), "hello".into())]);
        let codex = r#"{"response_item":{"payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}]}}}"#;
        assert_eq!(parse_codex(codex), vec![("codex".into(), "done".into())]);
    }

    #[test]
    fn claude_lookup_stops_after_two_project_directory_levels() {
        let root = std::env::temp_dir().join(format!(
            "herdr-claude-lookup-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let shallow = root.join("one/two/session.jsonl");
        let deep = root.join("one/two/three/session.jsonl");
        std::fs::create_dir_all(shallow.parent().unwrap()).unwrap();
        std::fs::create_dir_all(deep.parent().unwrap()).unwrap();
        std::fs::write(&shallow, "").unwrap();
        std::fs::write(&deep, "").unwrap();
        let mut entries_seen = 0;
        assert_eq!(
            find_named_file(&root, "session.jsonl", 2, &mut entries_seen),
            Some(shallow)
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
