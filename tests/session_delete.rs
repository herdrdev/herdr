use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

/// Owns a unique configuration directory; only child CLIs receive its environment.
struct SessionConfig {
    root: PathBuf,
}

impl SessionConfig {
    fn new() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        // Keep Unix socket paths below the host's length limit.
        #[cfg(unix)]
        let base = PathBuf::from("/tmp");
        #[cfg(not(unix))]
        let base = std::env::temp_dir();
        let root = base.join(format!(
            "hs-delete-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_micros(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self { root }
    }

    fn session_dir(&self, name: &str) -> PathBuf {
        let app_dir = if cfg!(debug_assertions) {
            "herdr-dev"
        } else {
            "herdr"
        };
        self.root.join(app_dir).join("sessions").join(name)
    }

    fn create_session(&self, name: &str) -> PathBuf {
        let dir = self.session_dir(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("session.json"), b"saved session").unwrap();
        dir
    }

    fn delete(&self, name: &str) -> Output {
        Command::new(env!("CARGO_BIN_EXE_herdr"))
            .args(["session", "delete", name, "--json"])
            .env("XDG_CONFIG_HOME", &self.root)
            .env_remove("HERDR_SESSION")
            .env_remove("HERDR_SOCKET_PATH")
            .env_remove("HERDR_CLIENT_SOCKET_PATH")
            .output()
            .unwrap()
    }
}

impl Drop for SessionConfig {
    fn drop(&mut self) {
        // Cleanup must not cause a second panic if a test assertion failed.
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn assert_success(output: &Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["deleted"], true);
    response
}

#[test]
fn delete_session_preserves_differently_cased_name() {
    let config = SessionConfig::new();
    let dir = config.create_session("Foo");
    let case_insensitive = config.session_dir("foo").exists();

    let output = config.delete("foo");

    assert_eq!(
        fs::read(dir.join("session.json")).unwrap(),
        b"saved session"
    );
    if case_insensitive {
        assert_eq!(output.status.code(), Some(1));
        let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error"]["code"], "session_delete_failed");
        assert!(error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("exact"));
    } else {
        assert_success(&output);
    }
}

#[test]
fn delete_session_removes_exact_stopped_name_only() {
    let config = SessionConfig::new();
    let dir = config.create_session("Foo");
    let other = config.create_session("other");

    let response = assert_success(&config.delete("Foo"));

    assert_eq!(response["session"]["name"], "Foo");
    assert_eq!(response["session"]["running"], false);
    assert!(!dir.exists());
    assert_eq!(
        fs::read(other.join("session.json")).unwrap(),
        b"saved session"
    );
}

#[test]
fn delete_session_missing_name_remains_idempotent() {
    let config = SessionConfig::new();
    assert_success(&config.delete("missing"));
    config.create_session("other");
    assert_success(&config.delete("missing"));
}

#[test]
fn delete_session_distinguishes_case_sensitive_siblings() {
    let config = SessionConfig::new();
    let upper = config.create_session("Foo");
    if config.session_dir("foo").exists() {
        return; // This filesystem cannot store both spellings independently.
    }
    let lower = config.create_session("foo");

    assert_success(&config.delete("foo"));

    assert!(!lower.exists());
    assert_eq!(
        fs::read(upper.join("session.json")).unwrap(),
        b"saved session"
    );
}

#[cfg(unix)]
#[test]
fn delete_session_preserves_running_session() {
    let config = SessionConfig::new();
    let dir = config.create_session("Foo");
    let _listener = std::os::unix::net::UnixListener::bind(dir.join("herdr.sock")).unwrap();

    let output = config.delete("Foo");

    assert_eq!(output.status.code(), Some(1));
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert!(error["error"]["message"]
        .as_str()
        .unwrap()
        .contains("running"));
    assert_eq!(
        fs::read(dir.join("session.json")).unwrap(),
        b"saved session"
    );
}

#[test]
fn session_config_cleans_up_after_panic() {
    let mut root = None;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let config = SessionConfig::new();
        root = Some(config.root.clone());
        config.create_session("Foo");
        panic!("simulate a failed assertion");
    }));

    assert!(result.is_err());
    assert!(!root.unwrap().exists());
}
