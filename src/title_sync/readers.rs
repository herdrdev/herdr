use std::fs;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use rusqlite::{Connection, OpenFlags};
use serde_json::Value;

use super::policy::{sanitize_title, PaneInput};

pub(crate) fn claude_title(session_id: &str, root: Option<&Path>) -> io::Result<Option<String>> {
    let root = root
        .map(Path::to_path_buf)
        .unwrap_or_else(|| home_root("CLAUDE_CONFIG_DIR", ".claude"));
    let projects = root.join("projects");
    let Ok(entries) = fs::read_dir(projects) else {
        return Ok(None);
    };
    let transcript = entries
        .filter_map(Result::ok)
        .find(|entry| {
            entry.file_type().is_ok_and(|kind| kind.is_dir())
                && entry.path().join(format!("{session_id}.jsonl")).is_file()
        })
        .map(|entry| entry.path().join(format!("{session_id}.jsonl")));
    let Some(transcript) = transcript else {
        return Ok(None);
    };
    let mut title = None;
    let mut custom = false;
    read_json_lines(&transcript, |row| {
        if row
            .get("sessionId")
            .and_then(Value::as_str)
            .is_some_and(|id| id != session_id)
        {
            return;
        }
        match row.get("type").and_then(Value::as_str) {
            Some("custom-title") => {
                custom = true;
                title = sanitize_title(row.get("customTitle").and_then(Value::as_str), 200);
            }
            Some("ai-title") if !custom => {
                title = sanitize_title(row.get("aiTitle").and_then(Value::as_str), 200);
            }
            _ => {}
        }
    })?;
    Ok(title)
}

pub(crate) fn codex_title(
    session_id: Option<&str>,
    root: Option<&Path>,
    pane: &PaneInput,
) -> io::Result<Option<String>> {
    let session_id = session_id
        .map(str::to_string)
        .or_else(|| resume_session_id(pane));
    let Some(session_id) = session_id else {
        return Ok(None);
    };
    let root = root
        .map(Path::to_path_buf)
        .unwrap_or_else(|| home_root("CODEX_HOME", ".codex"));
    let mut title = None;
    read_json_lines(&root.join("session_index.jsonl"), |row| {
        if row.get("id").and_then(Value::as_str) == Some(session_id.as_str()) {
            title = sanitize_title(row.get("thread_name").and_then(Value::as_str), 200);
        }
    })?;
    Ok(title)
}

pub(crate) fn kimi_title(session_id: &str, root: Option<&Path>) -> io::Result<Option<String>> {
    let root = root
        .map(Path::to_path_buf)
        .unwrap_or_else(|| home_root("KIMI_CODE_HOME", ".kimi-code"));
    let mut session_dir = None;
    read_json_lines(&root.join("session_index.jsonl"), |row| {
        if row.get("sessionId").and_then(Value::as_str) == Some(session_id) {
            session_dir = row
                .get("sessionDir")
                .and_then(Value::as_str)
                .map(PathBuf::from);
        }
    })?;
    let Some(session_dir) = session_dir.and_then(|path| trusted_child(&root, &path)) else {
        return Ok(None);
    };
    if let Ok(bytes) = fs::read(session_dir.join("state.json")) {
        if let Ok(state) = serde_json::from_slice::<Value>(&bytes) {
            if let Some(title) = sanitize_title(state.get("title").and_then(Value::as_str), 200) {
                return Ok(Some(title));
            }
        }
    }
    let mut first_prompt = None;
    read_json_lines(&session_dir.join("agents/main/wire.jsonl"), |row| {
        if first_prompt.is_some()
            || row.get("type").and_then(Value::as_str) != Some("turn.prompt")
            || row.pointer("/origin/kind").and_then(Value::as_str) != Some("user")
        {
            return;
        }
        first_prompt = row
            .get("input")
            .and_then(Value::as_array)
            .and_then(|parts| {
                parts
                    .iter()
                    .find_map(|part| part.get("text").and_then(Value::as_str))
            })
            .and_then(|text| sanitize_title(Some(text), 80));
    })?;
    Ok(first_prompt)
}

pub(crate) fn opencode_title(
    session_id: &str,
    path: Option<&Path>,
) -> rusqlite::Result<Option<String>> {
    let path = path.map(Path::to_path_buf).or_else(find_opencode_database);
    let Some(path) = path.filter(|path| path.is_file()) else {
        return Ok(None);
    };
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut statement = connection.prepare("SELECT title FROM session WHERE id = ?1")?;
    let mut rows = statement.query([session_id])?;
    Ok(rows
        .next()?
        .and_then(|row| row.get::<_, String>(0).ok())
        .and_then(|title| sanitize_title(Some(&title), 200)))
}

fn read_json_lines(path: &Path, mut visit: impl FnMut(&Value)) -> io::Result<()> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        if let Ok(row) = serde_json::from_str(&line) {
            visit(&row);
        }
    }
    Ok(())
}

fn resume_session_id(pane: &PaneInput) -> Option<String> {
    let detected;
    let processes = if pane.foreground_processes.is_empty() {
        detected = pane
            .shell_pid
            .and_then(crate::detect::foreground_job)
            .map(|job| {
                job.processes
                    .into_iter()
                    .map(|process| super::ProcessInput {
                        name: process.name,
                        argv: process.argv.unwrap_or_default(),
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        &detected
    } else {
        &pane.foreground_processes
    };
    processes.iter().find_map(|process| {
        if process.name != "codex" {
            return None;
        }
        let resume = process
            .argv
            .iter()
            .position(|argument| argument == "resume")?;
        let candidate = process.argv.get(resume + 1)?;
        is_session_id(candidate).then(|| candidate.clone())
    })
}

fn is_session_id(value: &str) -> bool {
    let groups = [8, 4, 4, 4, 12];
    let mut parts = value.split('-');
    groups.into_iter().all(|len| {
        parts.next().is_some_and(|part| {
            part.len() == len && part.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
    }) && parts.next().is_none()
}

fn trusted_child(root: &Path, candidate: &Path) -> Option<PathBuf> {
    let root = fs::canonicalize(root).ok()?;
    let child = fs::canonicalize(if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        root.join(candidate)
    })
    .ok()?;
    (child != root && child.starts_with(&root)).then_some(child)
}

fn home_root(env_name: &str, suffix: &str) -> PathBuf {
    std::env::var_os(env_name)
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(suffix))
        })
        .unwrap_or_else(|| PathBuf::from(suffix))
}

fn find_opencode_database() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("OPENCODE_DB_PATH").map(PathBuf::from) {
        return Some(path);
    }
    static CACHE: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();
    find_opencode_database_with(
        CACHE.get_or_init(|| Mutex::new(None)),
        Path::new("opencode"),
    )
}

fn find_opencode_database_with(cache: &Mutex<Option<PathBuf>>, command: &Path) -> Option<PathBuf> {
    if let Some(path) = cache.lock().ok()?.clone() {
        return Some(path);
    }
    let reported = Command::new(command)
        .args(["db", "path"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let path = reported.or_else(|| {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .map(|home| home.join(".local/share"))
            })
            .map(|root| root.join("opencode/opencode.db"))
    });
    *cache.lock().ok()? = path.clone();
    path
}

#[cfg(test)]
pub(super) mod tests_support {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    pub(crate) fn fixture_dir(name: &str) -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "vimeflow-title-reader-{}-{name}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).expect("fixture directory");
        path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::title_sync::ProcessInput;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn reads_claude_custom_title_without_later_ai_overwrite() {
        let root = tests_support::fixture_dir("claude");
        let project = root.join("projects/project");
        fs::create_dir_all(&project).expect("project");
        fs::write(project.join("session-1.jsonl"), "{\"type\":\"ai-title\",\"sessionId\":\"session-1\",\"aiTitle\":\"Generated\"}\n{\"type\":\"custom-title\",\"sessionId\":\"session-1\",\"customTitle\":\"Mine\"}\n{\"type\":\"ai-title\",\"sessionId\":\"session-1\",\"aiTitle\":\"Later\"}\n").expect("transcript");
        assert_eq!(
            claude_title("session-1", Some(&root))
                .expect("title")
                .as_deref(),
            Some("Mine")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn reads_latest_codex_name_and_exact_resume_process() {
        let root = tests_support::fixture_dir("codex");
        fs::write(root.join("session_index.jsonl"), "{\"id\":\"session-1\",\"thread_name\":\"First\"}\n{\"id\":\"other\",\"thread_name\":\"Wrong\"}\n{\"id\":\"session-1\",\"thread_name\":\"Latest\"}\n{\"id\":\"019fc2f1-7e03-7ec3-b35f-6546036e7616\",\"thread_name\":\"Worktree task\"}\n").expect("index");
        assert_eq!(
            codex_title(Some("session-1"), Some(&root), &PaneInput::default())
                .expect("title")
                .as_deref(),
            Some("Latest")
        );
        let pane = PaneInput {
            foreground_processes: vec![ProcessInput {
                name: "codex".into(),
                argv: vec![
                    "codex".into(),
                    "resume".into(),
                    "019fc2f1-7e03-7ec3-b35f-6546036e7616".into(),
                ],
            }],
            ..PaneInput::default()
        };
        assert_eq!(
            codex_title(None, Some(&root), &pane)
                .expect("resume title")
                .as_deref(),
            Some("Worktree task")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn reads_kimi_stored_title_and_first_user_prompt() {
        let root = tests_support::fixture_dir("kimi");
        let stored = root.join("sessions/stored");
        let fallback = root.join("sessions/fallback");
        fs::create_dir_all(stored.join("agents/main")).expect("stored");
        fs::create_dir_all(fallback.join("agents/main")).expect("fallback");
        fs::write(stored.join("state.json"), "{\"title\":\"Stored title\"}").expect("state");
        fs::write(fallback.join("state.json"), "{}").expect("state");
        fs::write(fallback.join("agents/main/wire.jsonl"), "{\"type\":\"turn.prompt\",\"origin\":{\"kind\":\"user\"},\"input\":[{\"type\":\"text\",\"text\":\"Fallback title from the first prompt\"}]}\n").expect("wire");
        fs::write(root.join("session_index.jsonl"), format!("{{\"sessionId\":\"stored\",\"sessionDir\":{}}}\n{{\"sessionId\":\"fallback\",\"sessionDir\":{}}}\n", serde_json::to_string(&stored).expect("path"), serde_json::to_string(&fallback).expect("path"))).expect("index");
        assert_eq!(
            kimi_title("stored", Some(&root))
                .expect("stored")
                .as_deref(),
            Some("Stored title")
        );
        assert_eq!(
            kimi_title("fallback", Some(&root))
                .expect("fallback")
                .as_deref(),
            Some("Fallback title from the first prompt")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn reads_opencode_title_and_caches_command_discovery() {
        let root = tests_support::fixture_dir("opencode");
        let database = root.join("opencode.db");
        let connection = Connection::open(&database).expect("database");
        connection.execute_batch("CREATE TABLE session (id TEXT PRIMARY KEY, title TEXT NOT NULL); INSERT INTO session VALUES ('session-1', 'Open title');").expect("schema");
        drop(connection);
        assert_eq!(
            opencode_title("session-1", Some(&database))
                .expect("title")
                .as_deref(),
            Some("Open title")
        );

        let command = root.join("opencode");
        fs::write(
            &command,
            format!("#!/bin/sh\nprintf '%s\\n' '{}'\n", database.display()),
        )
        .expect("command");
        fs::set_permissions(&command, fs::Permissions::from_mode(0o755)).expect("executable");
        let cache = Mutex::new(None);
        assert_eq!(
            find_opencode_database_with(&cache, &command).as_deref(),
            Some(database.as_path())
        );
        fs::write(&command, "#!/bin/sh\nexit 1\n").expect("replace command");
        assert_eq!(
            find_opencode_database_with(&cache, &command).as_deref(),
            Some(database.as_path())
        );
        let _ = fs::remove_dir_all(root);
    }
}
