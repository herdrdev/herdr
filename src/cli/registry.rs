//! Offline validation and explicit selected-session registry control.
use std::io;
use std::path::Path;

use crate::api::schema::{
    EmptyParams, Method, RegistryReloadParams, RegistryUpdateParams, Request,
};

use crate::agents::files::read_source;

const USAGE: &str = "usage: herdr registry validate <directory> | validate-snapshot <file> [--runtime-compatible] | status | reload [directory] | check [--channel stable|preview|staging] | update [--channel stable|preview|staging] | reset";

pub(super) fn run_registry_command(args: &[String]) -> io::Result<i32> {
    match args {
        [command, rest @ ..] if command == "check" || command == "update" => {
            if matches!(rest, [help] if is_help(help)) {
                print_help();
                return Ok(0);
            }
            let channel = match rest {
                [] => crate::agents::remote::Channel::Stable,
                [flag, value] if flag == "--channel" => match value.as_str() {
                    "stable" => crate::agents::remote::Channel::Stable,
                    "preview" => crate::agents::remote::Channel::Preview,
                    "staging" => crate::agents::remote::Channel::Staging,
                    _ => {
                        eprintln!("{USAGE}");
                        return Ok(2);
                    }
                },
                _ => {
                    eprintln!("{USAGE}");
                    return Ok(2);
                }
            };
            let params = RegistryUpdateParams { channel };
            return send_registry_request(if command == "check" {
                Method::RegistryCheck(params)
            } else {
                Method::RegistryUpdate(params)
            });
        }
        [command] if command == "reset" => {
            return send_registry_request(Method::RegistryReset(EmptyParams::default()));
        }
        [command, rest @ ..] if command == "validate-snapshot" => {
            if matches!(rest, [help] if is_help(help)) {
                print_help();
                return Ok(0);
            }
            let (file, runtime_compatible) = match rest {
                [file] if !file.starts_with('-') => (file, false),
                [file, flag] if !file.starts_with('-') && flag == "--runtime-compatible" => {
                    (file, true)
                }
                _ => {
                    eprintln!("{USAGE}");
                    return Ok(2);
                }
            };
            return match validate_snapshot(Path::new(file), runtime_compatible) {
                Ok(digest) => {
                    println!("agent registry snapshot valid: {digest} (not activated)");
                    Ok(0)
                }
                Err(error) => {
                    eprintln!("registry snapshot validation failed: {error}");
                    Ok(1)
                }
            };
        }
        [command] if command == "status" => {
            return send_registry_request(Method::RegistryStatus(EmptyParams::default()));
        }
        [command, rest @ ..] if command == "reload" => {
            if matches!(rest, [help] if is_help(help)) {
                print_help();
                return Ok(0);
            }
            let directory = match rest {
                [] => None,
                [directory] if !directory.is_empty() && !directory.starts_with('-') => {
                    Some(directory)
                }
                [separator, directory] if separator == "--" && !directory.is_empty() => {
                    Some(directory)
                }
                _ => {
                    eprintln!("{USAGE}");
                    return Ok(2);
                }
            };
            let source = match directory
                .map(|path| caller_source_path(path.as_str()))
                .transpose()
            {
                Ok(source) => source,
                Err(error) => {
                    eprintln!("registry reload failed: {error}");
                    return Ok(2);
                }
            };
            return send_registry_request(Method::RegistryReload(RegistryReloadParams { source }));
        }
        [command, help] if matches!(command.as_str(), "status" | "reset") && is_help(help) => {
            print_help();
            return Ok(0);
        }
        _ => {}
    }
    let directory = match args {
        [help] if is_help(help) => {
            print_help();
            return Ok(0);
        }
        [command, help] if command == "validate" && is_help(help) => {
            print_help();
            return Ok(0);
        }
        [command, directory]
            if command == "validate" && !directory.is_empty() && !directory.starts_with('-') =>
        {
            directory
        }
        [command, separator, directory]
            if command == "validate" && separator == "--" && !directory.is_empty() =>
        {
            directory
        }
        _ => {
            eprintln!("{USAGE}");
            return Ok(2);
        }
    };
    match validate_directory(Path::new(directory)) {
        Ok(count) => {
            println!("agent registry: {count} package(s) valid (not activated)");
            Ok(0)
        }
        Err(error) => {
            eprintln!("registry validation failed: {error}");
            Ok(1)
        }
    }
}

fn send_registry_request(method: Method) -> io::Result<i32> {
    super::print_response(&super::send_request(&Request {
        id: "cli:registry".into(),
        method,
    })?)
}

// Resolve once in the caller, without reading or writing the registry locally.
fn caller_source_path(source: &str) -> io::Result<String> {
    if source.contains("://") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "registry source must be a local directory",
        ));
    }
    let path = Path::new(source);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let source = absolute.into_os_string().into_string().map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidInput, "registry source must be UTF-8")
    })?;
    RegistryReloadParams {
        source: Some(source.clone()),
    }
    .source_path()
    .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    Ok(source)
}

fn is_help(value: &str) -> bool {
    matches!(value, "help" | "--help" | "-h")
}

fn print_help() {
    eprintln!("{USAGE}");
    eprintln!(
        "Validate local agents/ packages offline; status/reload target the selected session."
    );
    eprintln!(
        "Reload is offline. Check validates R2 without activation; update activates it explicitly."
    );
    eprintln!(
        "Reset returns to bundled/managed mode without modifying local overrides or source files."
    );
}

fn validate_snapshot(path: &Path, runtime_compatible: bool) -> Result<String, String> {
    let text = crate::agents::files::read_capped(
        crate::agents::files::open_regular(path)?,
        crate::agents::remote::MAX_SNAPSHOT_BYTES as u64,
    )?;
    let snapshot = crate::agents::remote::validate_snapshot(text.as_bytes())?;
    if runtime_compatible {
        let borrowed: Vec<_> = snapshot
            .files
            .iter()
            .map(|(p, t)| (p.as_str(), t.as_str()))
            .collect();
        let packages = crate::agents::validate_packages(&borrowed)?;
        crate::agents::store::validate_integration_baseline(&packages)?;
    }
    Ok(snapshot.content_sha256)
}

fn validate_directory(root: &Path) -> Result<usize, String> {
    let files = read_source(root)?;
    let borrowed: Vec<_> = files
        .iter()
        .map(|(path, text)| (path.as_str(), text.as_str()))
        .collect();
    crate::agents::validate_packages(&borrowed).map(|packages| packages.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    const AGENT: &str = "schema = 1\nid = 'future-agent'\nname = 'Future agent'\naliases = []\nstartable = true\n[launch]\nunix = 'future-agent'\nwindows = 'future-agent.cmd'\n";
    const DETECTION: &str = "id = 'future-agent'\nversion = '2026.01.01.1'\nmin_engine_version = 1\nupdated_at = '2026-01-01T00:00:00Z'\n[[rules]]\nid = 'idle'\nstate = 'idle'\ncontains = ['ready']\n";

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            #[cfg(unix)]
            let base = PathBuf::from("/var/tmp");
            #[cfg(not(unix))]
            let base = std::env::temp_dir();
            let path = base.join(format!(
                "herdr-registry-test-{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            fs::create_dir(&path).unwrap();
            let fixture = Self(path);
            fixture.put("agents/future-agent/agent.toml", AGENT.as_bytes());
            fixture
        }

        fn put(&self, relative: &str, bytes: &[u8]) -> PathBuf {
            let path = self.0.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, bytes).unwrap();
            path
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn registry_validate_routes_unknown_packages_without_activating_them() {
        let fixture = Fixture::new();
        fixture.put("agents/future-agent/detection.toml", DETECTION.as_bytes());
        let config = fixture.put("config.toml", b"untouched invalid config");
        assert!(crate::agents::registry()
            .profile_by_id("future-agent")
            .is_none());
        let outcome = super::super::maybe_run(&args(&[
            "herdr",
            "registry",
            "validate",
            fixture.0.to_str().unwrap(),
        ]))
        .unwrap();
        assert!(matches!(outcome, super::super::CommandOutcome::Handled(0)));
        assert_eq!(fs::read(config).unwrap(), b"untouched invalid config");
        assert!(crate::agents::registry()
            .profile_by_id("future-agent")
            .is_none());
    }

    #[test]
    fn registry_validate_rejects_invalid_package_and_actual_detection_regex() {
        let fixture = Fixture::new();
        fixture.put("agents/future-agent/agent.toml", b"schema = 999");
        assert!(validate_directory(&fixture.0).is_err());
        fixture.put("agents/future-agent/agent.toml", AGENT.as_bytes());
        fixture.put(
            "agents/future-agent/detection.toml",
            DETECTION
                .replace("contains = ['ready']", "regex = ['[']")
                .as_bytes(),
        );
        assert!(validate_directory(&fixture.0).is_err());
        assert_eq!(
            run_registry_command(&args(&["validate", fixture.0.to_str().unwrap(),])).unwrap(),
            1
        );
    }

    #[test]
    fn registry_validate_counts_packages_and_enforces_cross_package_identity_rules() {
        let fixture = Fixture::new();
        let second = AGENT.replace("future-agent", "another-agent");
        fixture.put("agents/another-agent/agent.toml", second.as_bytes());
        assert_eq!(validate_directory(&fixture.0).unwrap(), 2);
        fixture.put(
            "agents/another-agent/agent.toml",
            second
                .replace("aliases = []", "aliases = ['future-agent']")
                .as_bytes(),
        );
        assert!(validate_directory(&fixture.0).is_err());
    }

    #[test]
    fn registry_reload_resolves_source_in_caller_without_filesystem_access() {
        let relative = "nonexistent-registry-source";
        let source = caller_source_path(relative).unwrap();
        assert_eq!(
            Path::new(&source),
            std::env::current_dir().unwrap().join(relative)
        );
        assert_eq!(caller_source_path(&source).unwrap(), source);
        assert!(caller_source_path("https://example.com/registry").is_err());
    }

    #[test]
    fn registry_status_reload_help_and_argument_errors() {
        for values in [&["status", "--help"][..], &["reload", "-h"]] {
            assert_eq!(run_registry_command(&args(values)).unwrap(), 0);
        }
        for values in [
            &["status", "extra"][..],
            &["reload", "one", "two"],
            &["reload", "--unknown"],
            &["reload", ""],
            &["reload", "https://example.com/registry"],
        ] {
            assert_eq!(run_registry_command(&args(values)).unwrap(), 2);
        }
    }

    #[test]
    fn registry_validate_help_and_argument_errors() {
        for values in [&["help"][..], &["--help"], &["validate", "-h"]] {
            assert_eq!(run_registry_command(&args(values)).unwrap(), 0);
        }
        for values in [
            &[][..],
            &["validate"],
            &["install", "."],
            &["validate", "--unknown"],
            &["validate", ".", "extra"],
            &["validate", ""],
        ] {
            assert_eq!(run_registry_command(&args(values)).unwrap(), 2);
        }
        let fixture = Fixture::new();
        assert_eq!(
            run_registry_command(&args(&["validate", "--", fixture.0.to_str().unwrap(),])).unwrap(),
            0
        );
    }
}
