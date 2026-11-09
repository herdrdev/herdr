//! Named sessions: every session is its own server with its own socket. The list comes from the `shepherd`
//! binary (`shepherd session list --json`), which is where the session rules live.

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub name: String,
    pub default: bool,
    pub running: bool,
    /// The API socket (`.../shepherd.sock`) as the listing reports it.
    pub socket: PathBuf,
}

impl Session {
    /// The socket the endpoint client connects to, next to the API socket (`shepherd-client.sock`).
    pub fn client_socket(&self) -> PathBuf {
        client_socket_for(&self.socket)
    }
}

/// `…/shepherd.sock` → `…/shepherd-client.sock`.
pub fn client_socket_for(api: &Path) -> PathBuf {
    let stem = api
        .file_stem()
        .and_then(|n| n.to_str())
        .unwrap_or("shepherd")
        .to_owned();
    api.with_file_name(format!("{stem}-client.sock"))
}

/// Reads the output of `shepherd session list --json`.
pub fn parse(json: &str) -> Result<Vec<Session>, String> {
    let value: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let list = value
        .get("sessions")
        .and_then(|s| s.as_array())
        .ok_or("the session list has no `sessions`")?;
    Ok(list
        .iter()
        .filter_map(|s| {
            Some(Session {
                name: s.get("name")?.as_str()?.to_owned(),
                default: s.get("default").and_then(|d| d.as_bool()).unwrap_or(false),
                running: s.get("running").and_then(|r| r.as_bool()).unwrap_or(false),
                socket: PathBuf::from(s.get("socket_path")?.as_str()?),
            })
        })
        .collect())
}

/// The `shepherd` binary: `SHEPHERD_BIN` (the Makefile sets it) or the one on `PATH`.
fn binary() -> PathBuf {
    std::env::var_os("SHEPHERD_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("shepherd"))
}

/// Asks the binary for the sessions. Blocking: call it off the UI thread.
pub fn list() -> Result<Vec<Session>, String> {
    let output = Command::new(binary())
        .args(["session", "list", "--json"])
        .output()
        .map_err(|e| format!("could not run `shepherd session list`: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    parse(&String::from_utf8_lossy(&output.stdout))
}

fn run(args: &[&str]) -> Result<(), String> {
    let output = Command::new(binary())
        .args(args)
        .env_remove("SHEPHERD_SESSION")
        .output()
        .map_err(|e| format!("could not run `shepherd`: {e}"))?;
    if output.status.success() {
        return Ok(());
    }
    let why = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    Err(if why.is_empty() {
        format!("`shepherd {}` failed", args.join(" "))
    } else {
        why
    })
}

/// Stops a running session: its server ends and every terminal and agent in it closes. Blocking (up to
/// 15 s while the server shuts down): call it off the UI thread.
pub fn stop(name: &str) -> Result<(), String> {
    run(&["session", "stop", name])
}

/// Deletes a stopped session's folder (its saved state). The binary refuses `default` and running sessions.
pub fn delete(name: &str) -> Result<(), String> {
    run(&["session", "delete", name])
}

/// What the first click on a session's `×` warns about; the second click does it.
pub fn close_hint(session: &Session, current: bool) -> String {
    let name = &session.name;
    if !session.running {
        return format!("Click again to delete `{name}` and its saved state");
    }
    let here = if current { " (the one you are in)" } else { "" };
    format!("Click again to stop `{name}`{here}: its terminals and agents will close")
}

/// The same rules as the binary (`session::validate_name`), plus: not `default` and not one that exists.
pub fn validate_name(name: &str, existing: &[Session]) -> Result<(), String> {
    if name.is_empty() {
        return Err("give the session a name".into());
    }
    if name.len() > 64 {
        return Err("at most 64 characters".into());
    }
    if name == "." || name == ".." {
        return Err("the name cannot be . or ..".into());
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return Err("only letters, numbers, . _ and -".into());
    }
    if name == "default" || existing.iter().any(|s| s.name == name) {
        return Err(format!("`{name}` already exists"));
    }
    Ok(())
}

/// Starts a headless server for the named session (`shepherd --session <name> server`) and waits until the
/// session is listed as running, returning its client socket. Blocking: call it off the UI thread.
///
/// The server runs in its own process group so that it outlives this app. The variables that would point the
/// binary at another socket are removed, so the name is what decides where the session lives.
pub fn start(name: &str, wait: Duration) -> Result<PathBuf, String> {
    Command::new(binary())
        .args(["--session", name, "server"])
        .env_remove("SHEPHERD_SOCKET_PATH")
        .env_remove("SHEPHERD_CLIENT_SOCKET_PATH")
        .env_remove("SHEPHERD_SESSION")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|e| format!("could not run `shepherd`: {e}"))?;
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        if let Ok(list) = list() {
            if let Some(session) = list.iter().find(|s| s.name == name && s.running) {
                return Ok(session.client_socket());
            }
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    Err(format!("the server for `{name}` did not come up"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LISTING: &str = r#"{"sessions":[
        {"default":true,"name":"default","running":true,"session_dir":"/h/.config/shepherd-dev","socket_path":"/h/.config/shepherd-dev/shepherd.sock"},
        {"default":false,"name":"work","running":false,"session_dir":"/h/s/work","socket_path":"/h/s/work/shepherd.sock"},
        {"name":"broken"}]}"#;

    #[test]
    fn the_listing_is_read_and_entries_without_a_socket_are_skipped() {
        let sessions = parse(LISTING).unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].name, "default");
        assert!(sessions[0].default && sessions[0].running);
        assert!(!sessions[1].running);
        assert!(parse("{}").is_err());
        assert!(parse("not json").is_err());
    }

    #[test]
    fn the_client_socket_sits_next_to_the_api_socket() {
        let sessions = parse(LISTING).unwrap();
        assert_eq!(
            sessions[1].client_socket(),
            PathBuf::from("/h/s/work/shepherd-client.sock")
        );
    }

    #[test]
    fn a_new_name_follows_the_binarys_rules_and_cannot_clash() {
        let existing = parse(LISTING).unwrap();
        assert!(validate_name("review-2", &existing).is_ok());
        assert!(validate_name("a.b_c", &existing).is_ok());
        for bad in ["", "..", ".", "with space", "naïve", "a/b"] {
            assert!(validate_name(bad, &existing).is_err(), "{bad:?}");
        }
        assert!(validate_name(&"x".repeat(65), &existing).is_err());
        assert!(validate_name("default", &existing).is_err());
        assert!(validate_name("work", &existing).is_err(), "already listed");
    }

    #[test]
    fn the_warning_says_what_the_second_click_will_do() {
        let sessions = parse(LISTING).unwrap();
        let running = close_hint(&sessions[0], true);
        assert!(running.contains("stop `default`") && running.contains("you are in"));
        assert!(running.contains("terminals and agents will close"));
        let stopped = close_hint(&sessions[1], false);
        assert!(stopped.contains("delete `work`") && stopped.contains("saved state"));
    }
}
