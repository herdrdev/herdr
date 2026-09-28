//! Fallback source for an agent's model: the session's own log.
//!
//! Claude transcripts (`~/.claude/projects/<slug>/<session>.jsonl`) carry the
//! model on each assistant row (`{"type":"assistant","message":{"model":..}}`).
//! Codex rollouts (`<CODEX_HOME>/sessions/YYYY/MM/DD/rollout-*-<id>.jsonl`)
//! carry it on each turn context (`{"type":"turn_context","payload":{"model":..}}`).
//! Only the file tail is read, and only when the file changed since the last
//! read. Runs on the foreground-process refresh thread, never during render.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

/// Enough for several turns; a model switch shows up in the next row.
const TAIL_BYTES: u64 = 256 * 1024;
const ROLLOUT_MISS_RETRY: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ModelLogSource {
    ClaudeTranscript(PathBuf),
    CodexSession(String),
}

impl ModelLogSource {
    /// The agent whose log this is.
    pub(crate) fn agent(&self) -> crate::detect::Agent {
        match self {
            Self::ClaudeTranscript(_) => crate::detect::Agent::Claude,
            Self::CodexSession(_) => crate::detect::Agent::Codex,
        }
    }
}

#[derive(Default)]
struct Cache {
    /// path -> (len, mtime, model)
    tails: HashMap<PathBuf, (u64, Option<SystemTime>, Option<String>)>,
    /// codex session id -> rollout path, or the time a lookup missed
    rollouts: HashMap<String, Result<PathBuf, Instant>>,
}

fn cache() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(Mutex::default)
}

/// The model the session's log says is in use, or `None` if unknown.
pub(crate) fn model_from_log(source: &ModelLogSource) -> Option<String> {
    let mut cache = cache().lock().ok()?;
    let (path, kind) = match source {
        ModelLogSource::ClaudeTranscript(path) => (path.clone(), LogKind::Claude),
        ModelLogSource::CodexSession(id) => {
            let path = match cache.rollouts.get(id) {
                Some(Ok(path)) => path.clone(),
                Some(Err(missed)) if missed.elapsed() < ROLLOUT_MISS_RETRY => return None,
                _ => match find_codex_rollout(&codex_session_roots(), id) {
                    Some(path) => {
                        cache.rollouts.insert(id.clone(), Ok(path.clone()));
                        path
                    }
                    None => {
                        cache.rollouts.insert(id.clone(), Err(Instant::now()));
                        return None;
                    }
                },
            };
            (path, LogKind::Codex)
        }
    };
    let metadata = std::fs::metadata(&path).ok()?;
    let stamp = (metadata.len(), metadata.modified().ok());
    if let Some((len, mtime, model)) = cache.tails.get(&path) {
        if (*len, *mtime) == stamp {
            return model.clone();
        }
    }
    let model = read_tail(&path, TAIL_BYTES)
        .ok()
        .and_then(|tail| latest_model(&tail, kind));
    cache.tails.insert(path, (stamp.0, stamp.1, model.clone()));
    model
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LogKind {
    Claude,
    Codex,
}

/// The model named by the last matching row in a JSONL tail.
pub(crate) fn latest_model(tail: &[u8], kind: LogKind) -> Option<String> {
    tail.split(|byte| *byte == b'\n').rev().find_map(|line| {
        let value: serde_json::Value = serde_json::from_slice(line).ok()?;
        let model = match kind {
            LogKind::Claude if value.get("type")?.as_str()? == "assistant" => {
                value.get("message")?.get("model")?.as_str()?
            }
            LogKind::Codex if value.get("type")?.as_str()? == "turn_context" => {
                value.get("payload")?.get("model")?.as_str()?
            }
            _ => return None,
        };
        // Claude writes `<synthetic>` for locally generated rows.
        (!model.is_empty() && !model.starts_with('<')).then(|| model.to_string())
    })
}

fn read_tail(path: &Path, max: u64) -> std::io::Result<Vec<u8>> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(max)))?;
    let mut tail = Vec::new();
    file.take(max).read_to_end(&mut tail)?;
    Ok(tail)
}

fn codex_session_roots() -> Vec<PathBuf> {
    let mut homes = Vec::new();
    if let Ok(home) = crate::integration::codex_dir() {
        homes.push(home);
    }
    if let Ok(home) = crate::integration::home_dir() {
        homes.push(home.join(".codex"));
        if let Ok(profiles) = std::fs::read_dir(home.join(".codex-profiles")) {
            homes.extend(profiles.flatten().map(|entry| entry.path()));
        }
    }
    homes.dedup();
    homes
        .into_iter()
        .map(|home| home.join("sessions"))
        .collect()
}

/// Newest-first walk of `sessions/YYYY/MM/DD` for `rollout-*-<id>.jsonl`.
pub(crate) fn find_codex_rollout(roots: &[PathBuf], id: &str) -> Option<PathBuf> {
    if id.is_empty() || id.contains(['/', '\\']) {
        return None;
    }
    let suffix = format!("-{id}.jsonl");
    let sorted_dirs = |dir: &Path| -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
                    .map(|entry| entry.path())
                    .collect()
            })
            .unwrap_or_default();
        dirs.sort_unstable_by(|a, b| b.cmp(a));
        dirs
    };
    roots.iter().find_map(|root| {
        sorted_dirs(root).into_iter().find_map(|year| {
            sorted_dirs(&year).into_iter().find_map(|month| {
                sorted_dirs(&month).into_iter().find_map(|day| {
                    std::fs::read_dir(&day).ok()?.flatten().find_map(|entry| {
                        let name = entry.file_name();
                        let name = name.to_str()?;
                        (name.starts_with("rollout-") && name.ends_with(&suffix))
                            .then(|| entry.path())
                    })
                })
            })
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLAUDE_FIXTURE: &str = concat!(
        r#"{"type":"user","message":{"role":"user","content":"hi"}}"#,
        "\n",
        r#"{"type":"assistant","message":{"model":"claude-opus-5-5","role":"assistant"}}"#,
        "\n",
        r#"{"type":"user","message":{"role":"user","content":"/model fable"}}"#,
        "\n",
        r#"{"type":"assistant","message":{"model":"claude-fable-5-1","role":"assistant"}}"#,
        "\n",
        r#"{"type":"assistant","message":{"model":"<synthetic>","role":"assistant"}}"#,
        "\n",
        r#"{"type":"system","subtype":"turn_duration"}"#,
        "\n",
    );

    const CODEX_FIXTURE: &str = concat!(
        r#"{"type":"session_meta","payload":{"id":"01a0e7dc-574b-7241-b3d5-ce03bc60a3e8"}}"#,
        "\n",
        r#"{"type":"turn_context","payload":{"model":"gpt-6-luna","effort":"xhigh"}}"#,
        "\n",
        r#"{"type":"turn_context","payload":{"model":"gpt-6-astra","effort":"xhigh"}}"#,
        "\n",
        r#"{"type":"event_msg","payload":{"type":"token_count"}}"#,
        "\n",
    );

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-model-log-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn claude_uses_latest_assistant_model_after_a_switch() {
        assert_eq!(
            latest_model(CLAUDE_FIXTURE.as_bytes(), LogKind::Claude).as_deref(),
            Some("claude-fable-5-1")
        );
    }

    #[test]
    fn codex_uses_latest_turn_context_model() {
        assert_eq!(
            latest_model(CODEX_FIXTURE.as_bytes(), LogKind::Codex).as_deref(),
            Some("gpt-6-astra")
        );
        assert_eq!(
            latest_model(CODEX_FIXTURE.as_bytes(), LogKind::Claude),
            None
        );
    }

    #[test]
    fn a_truncated_first_line_is_skipped() {
        let tail = &CLAUDE_FIXTURE.as_bytes()[10..];
        assert_eq!(
            latest_model(tail, LogKind::Claude).as_deref(),
            Some("claude-fable-5-1")
        );
        assert_eq!(latest_model(b"{\"type\":\"assist", LogKind::Claude), None);
    }

    #[test]
    fn claude_transcript_file_is_read_and_follows_appends() {
        let dir = temp_dir("claude");
        let path = dir.join("session.jsonl");
        std::fs::write(&path, CLAUDE_FIXTURE).expect("write fixture");
        let source = ModelLogSource::ClaudeTranscript(path.clone());
        assert_eq!(model_from_log(&source).as_deref(), Some("claude-fable-5-1"));
        let mut appended = CLAUDE_FIXTURE.to_string();
        appended.push_str(r#"{"type":"assistant","message":{"model":"claude-opus-5-5"}}"#);
        appended.push('\n');
        std::fs::write(&path, appended).expect("append");
        assert_eq!(model_from_log(&source).as_deref(), Some("claude-opus-5-5"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn codex_rollout_is_found_by_session_id() {
        let dir = temp_dir("codex");
        let day = dir.join("sessions/2026/09/28");
        std::fs::create_dir_all(&day).expect("day dir");
        let id = "01a0e7dc-574b-7241-b3d5-ce03bc60a3e8";
        let rollout = day.join(format!("rollout-2026-09-28T13-52-52-{id}.jsonl"));
        std::fs::write(&rollout, CODEX_FIXTURE).expect("write fixture");
        let roots = [dir.join("missing/sessions"), dir.join("sessions")];
        assert_eq!(find_codex_rollout(&roots, id), Some(rollout.clone()));
        assert_eq!(find_codex_rollout(&roots, "other"), None);
        assert_eq!(find_codex_rollout(&roots, "../x"), None);
        let tail = read_tail(&rollout, TAIL_BYTES).expect("tail");
        assert_eq!(
            latest_model(&tail, LogKind::Codex).as_deref(),
            Some("gpt-6-astra")
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
