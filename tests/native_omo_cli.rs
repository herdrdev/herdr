use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct Sandbox(PathBuf);

impl Sandbox {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "herdr-native-omo-cli-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_herdr"))
            .args(["integration"])
            .args(args)
            .env_clear()
            .env("HOME", &self.0)
            .env("USERPROFILE", &self.0)
            .env("XDG_CONFIG_HOME", self.0.join("config"))
            .env("XDG_STATE_HOME", self.0.join("state"))
            .env("OMO_CODING_AGENT_DIR", self.0.join("agent"))
            .env("PATH", &self.0)
            .output()
            .unwrap()
    }

    fn extension(&self) -> PathBuf {
        self.0.join("agent/extensions/herdr-omo.js")
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn native_omo_cli_install_status_and_uninstall_use_only_the_sandbox() {
    let sandbox = Sandbox::new();
    let missing = sandbox.run(&["status"]);
    assert!(missing.status.success());
    let stdout = String::from_utf8(missing.stdout).unwrap();
    assert!(stdout.contains("omo: not installed"));
    assert!(stdout.contains(&sandbox.extension().display().to_string()));

    let installed = sandbox.run(&["install", "omo"]);
    assert!(installed.status.success(), "{installed:?}");
    assert!(sandbox.extension().is_file());
    let bytes = std::fs::read(sandbox.extension()).unwrap();
    let current = sandbox.run(&["status"]);
    assert!(current.status.success());
    assert!(String::from_utf8(current.stdout)
        .unwrap()
        .contains("omo: current"));
    assert!(sandbox.run(&["install", "omo"]).status.success());
    assert_eq!(std::fs::read(sandbox.extension()).unwrap(), bytes);

    let unrelated = sandbox.extension().with_file_name("user-extension.js");
    std::fs::write(&unrelated, "user extension").unwrap();
    let removed = sandbox.run(&["uninstall", "omo"]);
    assert!(removed.status.success(), "{removed:?}");
    assert!(!sandbox.extension().exists());
    assert_eq!(
        std::fs::read_to_string(unrelated).unwrap(),
        "user extension"
    );
    assert!(sandbox.run(&["uninstall", "omo"]).status.success());
}

#[test]
fn native_omo_cli_reports_and_refuses_modified_and_foreign_files() {
    let sandbox = Sandbox::new();
    assert!(sandbox.run(&["install", "omo"]).status.success());
    let mut modified = std::fs::read(sandbox.extension()).unwrap();
    modified.extend_from_slice(b"\n// user modification\n");
    std::fs::write(sandbox.extension(), &modified).unwrap();
    let status = sandbox.run(&["status"]);
    assert!(status.status.success());
    assert!(String::from_utf8(status.stdout)
        .unwrap()
        .contains("omo: modified"));
    for action in ["install", "uninstall"] {
        assert_eq!(sandbox.run(&[action, "omo"]).status.code(), Some(1));
        assert_eq!(std::fs::read(sandbox.extension()).unwrap(), modified);
    }
    std::fs::write(sandbox.extension(), "foreign extension").unwrap();
    let status = sandbox.run(&["status"]);
    assert!(status.status.success());
    assert!(String::from_utf8(status.stdout)
        .unwrap()
        .contains("omo: foreign"));
    for action in ["install", "uninstall"] {
        assert_eq!(sandbox.run(&[action, "omo"]).status.code(), Some(1));
        assert_eq!(
            std::fs::read_to_string(sandbox.extension()).unwrap(),
            "foreign extension"
        );
    }
}

#[test]
fn native_omo_cli_rejects_unknown_targets_without_installing() {
    let sandbox = Sandbox::new();
    for action in ["install", "uninstall"] {
        assert_eq!(
            sandbox.run(&[action, "unknown-agent"]).status.code(),
            Some(2)
        );
    }
    assert!(!sandbox.extension().exists());
}
