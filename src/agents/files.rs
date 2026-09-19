//! Bounded, read-only agent source ingestion shared by local validation and reload.

use std::fs::{self, File, Metadata, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};

pub(crate) const MAX_FILES: usize = 4096;
const MAX_ENTRIES: usize = 8192;
pub(crate) const MAX_FILE_BYTES: u64 = 1024 * 1024;
pub(crate) const MAX_TOTAL_BYTES: u64 = 32 * 1024 * 1024;
const MAX_DEPTH: usize = 4;

#[derive(Debug)]
struct SourceFile {
    path: PathBuf,
    relative: String,
}

#[derive(Default)]
struct Inventory {
    files: Vec<SourceFile>,
    entries: usize,
    bytes: u64,
}

fn metadata(path: &Path) -> Result<Metadata, String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| at(path, error))?;
    if metadata.file_type().is_symlink() {
        return Err(at(path, "symlinks are forbidden"));
    }
    Ok(metadata)
}

fn require_directory(path: &Path) -> Result<(), String> {
    if !metadata(path)?.is_dir() {
        return Err(at(path, "expected a directory"));
    }
    Ok(())
}

fn check_file(path: &Path, metadata: &Metadata, remaining: u64) -> Result<(), String> {
    if !metadata.is_file() {
        return Err(at(path, "expected a regular file"));
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err(at(path, "file exceeds 1 MiB limit"));
    }
    if metadata.len() > remaining {
        return Err(at(path, "source exceeds 32 MiB total limit"));
    }
    Ok(())
}

fn inventory(
    directory: &Path,
    relative: &str,
    depth: usize,
    result: &mut Inventory,
) -> Result<(), String> {
    // Do not collect a directory iterator before enforcing the entry bound.
    for entry in fs::read_dir(directory).map_err(|error| at(directory, error))? {
        result.entries += 1;
        if result.entries > MAX_ENTRIES {
            return Err("source exceeds directory entry limit".into());
        }
        let entry = entry.map_err(|error| at(directory, error))?;
        let path = entry.path();
        if depth + 1 > MAX_DEPTH {
            return Err(at(&path, "source exceeds maximum path depth of 4"));
        }
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| at(&path, "path is not UTF-8"))?;
        if name.contains('\\') || name.chars().any(char::is_control) {
            return Err(at(&path, "unsafe source path"));
        }
        let relative = format!("{relative}/{name}");
        if relative.len() > 256 {
            return Err(at(&path, "source path too long"));
        }
        let metadata = metadata(&path)?;
        if metadata.is_dir() {
            inventory(&path, &relative, depth + 1, result)?;
        } else {
            if result.files.len() >= MAX_FILES {
                return Err("source exceeds 4096 file limit".into());
            }
            check_file(&path, &metadata, MAX_TOTAL_BYTES - result.bytes)?;
            result.bytes += metadata.len();
            result.files.push(SourceFile { path, relative });
        }
    }
    Ok(())
}

pub(crate) fn read_source(root: &Path) -> Result<Vec<(String, String)>, String> {
    require_directory(root)?;
    let agents = root.join("agents");
    require_directory(&agents)?;
    // Check the complete inventory's counts and declared byte sizes before
    // reading any content. Ignore everything outside the agents/ subtree.
    let mut found = Inventory::default();
    inventory(&agents, "agents", 1, &mut found)?;
    found
        .files
        .sort_by(|left, right| left.relative.cmp(&right.relative));
    let mut files = Vec::with_capacity(found.files.len());
    let mut total = 0;
    for source in found.files {
        let remaining = MAX_TOTAL_BYTES - total;
        check_file(&source.path, &metadata(&source.path)?, remaining)?;
        let file = open_regular(&source.path)?;
        check_file(
            &source.path,
            &file.metadata().map_err(|error| at(&source.path, error))?,
            remaining,
        )?;
        // A file may grow after metadata inspection. Never use read_to_string
        // without a cap, and enforce the aggregate bound on the actual bytes.
        let text = read_capped(file, MAX_FILE_BYTES.min(remaining))
            .map_err(|error| at(&source.path, error))?;
        total += text.len() as u64;
        files.push((source.relative, text));
    }
    Ok(files)
}

// Close the final-component symlink/FIFO race between inventory and open on
// Unix. Recheck the opened handle as well; file growth remains separately capped.
pub(crate) fn open_regular(path: &Path) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|error| at(path, error))?;
    if !file.metadata().map_err(|error| at(path, error))?.is_file() {
        return Err(at(path, "expected a regular file"));
    }
    Ok(file)
}

pub(crate) fn read_capped(reader: impl Read, limit: u64) -> Result<String, String> {
    let mut bytes = Vec::new();
    reader
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > limit {
        return Err("source grew beyond the file or total byte limit".into());
    }
    String::from_utf8(bytes).map_err(|_| "file is not UTF-8".into())
}

fn at(path: &Path, error: impl std::fmt::Display) -> String {
    format!("{}: {error}", path.display())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    const AGENT: &str = "schema = 1\nid = 'future-agent'\nname = 'Future agent'\naliases = []\nstartable = true\n[launch]\nunix = 'future-agent'\nwindows = 'future-agent.cmd'\n";

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

    fn validate_directory(root: &Path) -> Result<usize, String> {
        let files = read_source(root)?;
        let borrowed: Vec<_> = files
            .iter()
            .map(|(path, text)| (path.as_str(), text.as_str()))
            .collect();
        crate::agents::validate_packages(&borrowed).map(|packages| packages.len())
    }

    #[test]
    fn registry_reader_is_sorted_preserves_bytes_and_ignores_repository_metadata() {
        let fixture = Fixture::new();
        fixture.put("README.md", &[0xff]);
        fixture.put(".git/config", &[0xff]);
        fixture.put("agents/future-agent/assets/z.sh", b"#!/bin/sh\r\n");
        fixture.put("agents/future-agent/assets/a.py", "# café\n".as_bytes());
        let files = read_source(&fixture.0).unwrap();
        assert_eq!(
            files
                .iter()
                .map(|(path, _)| path.as_str())
                .collect::<Vec<_>>(),
            [
                "agents/future-agent/agent.toml",
                "agents/future-agent/assets/a.py",
                "agents/future-agent/assets/z.sh",
            ]
        );
        assert_eq!(files[2].1, "#!/bin/sh\r\n");
    }

    #[test]
    fn registry_reader_rejects_missing_empty_binary_and_deep_trees() {
        let fixture = Fixture::new();
        assert!(read_source(&fixture.0.join("missing")).is_err());
        let file = fixture.0.join("agents/future-agent/agent.toml");
        assert!(read_source(&file).is_err());
        fs::remove_file(&file).unwrap();
        assert!(validate_directory(&fixture.0).is_err());
        fixture.put("agents/future-agent/agent.toml", &[0xff]);
        assert!(read_source(&fixture.0).unwrap_err().contains("UTF-8"));
        fixture.put("agents/future-agent/assets/nested/hook.sh", b"text");
        assert!(read_source(&fixture.0).unwrap_err().contains("depth"));
    }

    #[test]
    fn registry_reader_caps_growth_after_metadata_and_rejects_non_utf8() {
        assert_eq!(read_capped(&b"text"[..], 4).unwrap(), "text");
        assert!(read_capped(&b"grown"[..], 4).unwrap_err().contains("limit"));
        assert!(read_capped(&b"\xff"[..], 1).unwrap_err().contains("UTF-8"));
    }

    #[test]
    fn registry_reader_rejects_oversized_files_before_reading_any_contents() {
        let fixture = Fixture::new();
        fixture.put("agents/future-agent/agent.toml", &[0xff]);
        let big = fixture.put("agents/future-agent/assets/big.sh", b"");
        File::options()
            .write(true)
            .open(big)
            .unwrap()
            .set_len(MAX_FILE_BYTES + 1)
            .unwrap();
        assert!(read_source(&fixture.0).unwrap_err().contains("1 MiB"));
    }

    #[test]
    fn registry_reader_bounds_aggregate_bytes_before_reading_any_contents() {
        let fixture = Fixture::new();
        for index in 0..32 {
            let big = fixture.put(&format!("agents/future-agent/assets/{index}.sh"), b"");
            File::options()
                .write(true)
                .open(big)
                .unwrap()
                .set_len(MAX_FILE_BYTES)
                .unwrap();
        }
        assert!(read_source(&fixture.0).unwrap_err().contains("32 MiB"));
    }

    #[test]
    fn registry_reader_bounds_file_count_before_collecting() {
        let fixture = Fixture::new();
        for index in 0..MAX_FILES {
            fixture.put(&format!("agents/future-agent/assets/{index}.sh"), b"");
        }
        assert!(read_source(&fixture.0).unwrap_err().contains("4096"));
    }

    #[test]
    fn registry_reader_bounds_source_paths_before_reading_content() {
        let fixture = Fixture::new();
        fixture.put(
            &format!("agents/future-agent/assets/{}", "x".repeat(240)),
            b"text",
        );
        assert!(read_source(&fixture.0)
            .unwrap_err()
            .contains("path too long"));
    }

    #[cfg(unix)]
    #[test]
    fn registry_reader_rejects_non_utf8_and_unsafe_source_names() {
        use std::os::unix::ffi::OsStringExt;
        let fixture = Fixture::new();
        let path = fixture
            .0
            .join("agents")
            .join(std::ffi::OsString::from_vec(vec![0xff]));
        match fs::write(&path, b"text") {
            Ok(()) => {
                assert!(read_source(&fixture.0).unwrap_err().contains("UTF-8"));
                fs::remove_file(path).unwrap();
            }
            // APFS rejects non-UTF-8 names before the reader can observe them.
            Err(error) => assert_eq!(error.raw_os_error(), Some(libc::EILSEQ)),
        }
        fixture.put("agents/future-agent/assets/unsafe\\name", b"text");
        assert!(read_source(&fixture.0)
            .unwrap_err()
            .contains("unsafe source path"));
    }

    #[cfg(unix)]
    #[test]
    fn registry_reader_rejects_root_agents_directory_and_file_symlinks() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let link = fixture.0.join("root-link");
        symlink(&fixture.0, &link).unwrap();
        assert!(read_source(&link).unwrap_err().contains("symlink"));
        fs::remove_file(&link).unwrap();
        let agents = fixture.0.join("agents");
        let saved = fixture.0.join("saved-agents");
        fs::rename(&agents, &saved).unwrap();
        symlink(&saved, &agents).unwrap();
        assert!(read_source(&fixture.0).unwrap_err().contains("symlink"));
        fs::remove_file(&agents).unwrap();
        fs::rename(&saved, &agents).unwrap();
        for target in [&agents, &agents.join("future-agent/agent.toml")] {
            let link = agents.join("link");
            symlink(target, &link).unwrap();
            assert!(read_source(&fixture.0).unwrap_err().contains("symlink"));
            fs::remove_file(&link).unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn registry_reader_rejects_nonregular_files_without_opening_them() {
        let fixture = Fixture::new();
        let socket = fixture.0.join("agents/socket");
        let _listener = std::os::unix::net::UnixListener::bind(socket).unwrap();
        assert!(read_source(&fixture.0)
            .unwrap_err()
            .contains("regular file"));
    }
}
