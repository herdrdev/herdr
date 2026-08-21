use std::path::PathBuf;

use tracing::warn;

use super::policy::{sanitize_title, PaneInput};
use super::readers;

#[derive(Clone, Debug, Default)]
pub(crate) struct ReaderPaths {
    pub(crate) claude_root: Option<PathBuf>,
    pub(crate) codex_root: Option<PathBuf>,
    pub(crate) kimi_root: Option<PathBuf>,
    pub(crate) opencode_db: Option<PathBuf>,
}

pub(crate) fn title_for_pane(pane: &PaneInput, paths: &ReaderPaths) -> Option<String> {
    let agent = pane
        .agent
        .as_deref()
        .or_else(|| pane.agent_session.as_ref()?.agent.as_deref())?
        .to_lowercase();
    let session_id = pane
        .agent_session
        .as_ref()
        .filter(|session| session.kind.as_deref() == Some("id"))
        .and_then(|session| session.value.as_deref());

    let durable: Option<Result<Option<String>, String>> = match agent.as_str() {
        "claude" => session_id.map(|id| {
            readers::claude_title(id, paths.claude_root.as_deref())
                .map_err(|error| error.to_string())
        }),
        "codex" => Some(
            readers::codex_title(session_id, paths.codex_root.as_deref(), pane)
                .map_err(|error| error.to_string()),
        ),
        "kimi" => session_id.map(|id| {
            readers::kimi_title(id, paths.kimi_root.as_deref()).map_err(|error| error.to_string())
        }),
        "opencode" => session_id.map(|id| {
            readers::opencode_title(id, paths.opencode_db.as_deref())
                .map_err(|error| error.to_string())
        }),
        _ => None,
    };
    match durable {
        Some(Ok(Some(title))) => Some(title),
        Some(Ok(None)) | None => terminal_title(pane, &agent),
        Some(Err(error)) => {
            warn!(agent, %error, "agent title lookup failed; using terminal title");
            terminal_title(pane, &agent)
        }
    }
}

fn terminal_title(pane: &PaneInput, agent: &str) -> Option<String> {
    let mut title = pane
        .terminal_title_stripped
        .as_deref()
        .or(pane.terminal_title.as_deref())?
        .trim();
    if title
        .chars()
        .next()
        .is_some_and(|character| matches!(character, '✳' | '✢' | '·' | '…'))
    {
        title = title
            .chars()
            .next()
            .map(|first| &title[first.len_utf8()..])?
            .trim_start();
    }
    if title
        .get(..2)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("oc"))
    {
        let rest = title[2..].trim_start();
        if let Some(stripped) = rest.strip_prefix('|') {
            title = stripped.trim_start();
        }
    }
    let title = sanitize_title(Some(title), 200)?;
    let lower = title.to_lowercase();
    if lower == agent || lower == format!("{agent} code") {
        return None;
    }
    let cwd_name = pane
        .foreground_cwd
        .as_deref()
        .or(pane.cwd.as_deref())
        .and_then(|cwd| cwd.file_name())
        .and_then(|name| name.to_str());
    (cwd_name != Some(title.as_str())).then_some(title)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::title_sync::{AgentSession, ProcessInput};

    fn pane(agent: Option<&str>, kind: Option<&str>, title: Option<&str>) -> PaneInput {
        PaneInput {
            pane_id: "w1:p1".into(),
            agent: agent.map(str::to_string),
            agent_session: Some(AgentSession {
                agent: Some("claude".into()),
                kind: kind.map(str::to_string),
                value: Some("session-1".into()),
            }),
            terminal_title: title.map(str::to_string),
            ..PaneInput::default()
        }
    }

    #[test]
    fn orchestration_ports_identity_gate_and_terminal_policy() {
        let cases = [
            (
                "future agents use terminal titles",
                pane(Some("future-agent"), None, Some("Current task")),
                Some("Current task"),
            ),
            (
                "session agent is an identity fallback",
                pane(None, Some("path"), Some("Current task")),
                Some("Current task"),
            ),
            (
                "decorations are stripped",
                pane(Some("opencode"), Some("path"), Some("✳ OC | Build UI")),
                Some("Build UI"),
            ),
            (
                "generic names are rejected",
                pane(Some("codex"), Some("path"), Some("Codex Code")),
                None,
            ),
        ];
        for (name, pane, expected) in cases {
            assert_eq!(
                title_for_pane(&pane, &ReaderPaths::default()).as_deref(),
                expected,
                "{name}"
            );
        }

        let mut cwd_echo = pane(Some("future-agent"), None, Some("project"));
        cwd_echo.foreground_cwd = Some("/tmp/project".into());
        assert_eq!(title_for_pane(&cwd_echo, &ReaderPaths::default()), None);
    }

    #[test]
    fn non_id_sessions_do_not_reach_durable_readers() {
        let root = readers::tests_support::fixture_dir("kind-gate");
        let mut input = pane(Some("claude"), Some("path"), Some("Fallback"));
        input.foreground_processes = vec![ProcessInput::default()];
        assert_eq!(
            title_for_pane(
                &input,
                &ReaderPaths {
                    claude_root: Some(root.clone()),
                    ..ReaderPaths::default()
                }
            )
            .as_deref(),
            Some("Fallback")
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn reader_failures_fall_back_to_terminal_title() {
        let root = readers::tests_support::fixture_dir("reader-error");
        let database = root.join("not-a-database");
        std::fs::write(&database, "broken").expect("database fixture");
        let mut input = pane(Some("opencode"), Some("id"), Some("Terminal fallback"));
        input.agent_session.as_mut().expect("session").agent = Some("opencode".into());
        assert_eq!(
            title_for_pane(
                &input,
                &ReaderPaths {
                    opencode_db: Some(database),
                    ..ReaderPaths::default()
                }
            )
            .as_deref(),
            Some("Terminal fallback")
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
