use super::*;

struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "herdr-config-write-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

// Windows existing files have separate native backup/recovery coverage.
#[cfg(unix)]
const ATOMIC_CASES: &[bool] = &[false, true];
#[cfg(windows)]
const ATOMIC_CASES: &[bool] = &[false];

#[test]
fn config_publication_keeps_old_content_until_commit() {
    let dir = Directory::new();
    for &existing in ATOMIC_CASES {
        let path = dir.0.join(if existing { "existing" } else { "new" });
        if existing {
            fs::write(&path, b"old preferences").unwrap();
        }
        let staged = Replacement::prepare(&path, b"complete new preferences").unwrap();
        if existing {
            assert_eq!(fs::read(&path).unwrap(), b"old preferences");
        } else {
            assert!(!path.exists());
        }
        assert_eq!(
            fs::read(&staged.temporary).unwrap(),
            b"complete new preferences"
        );
        staged.commit().unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"complete new preferences");
    }
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), ATOMIC_CASES.len());
}

#[test]
fn abandoned_and_failed_publication_leave_config_unchanged() {
    for &existing in ATOMIC_CASES {
        let dir = Directory::new();
        let path = dir.0.join("config");
        if existing {
            fs::write(&path, b"original").unwrap();
        }
        drop(Replacement::prepare(&path, b"new").unwrap());
        let staged = Replacement::prepare(&path, b"new").unwrap();
        fs::remove_file(&staged.temporary).unwrap();
        assert_eq!(staged.commit().unwrap_err().kind(), io::ErrorKind::NotFound);
        if existing {
            assert_eq!(fs::read(&path).unwrap(), b"original");
        } else {
            assert!(!path.exists());
        }
        assert_eq!(fs::read_dir(&dir.0).unwrap().count(), usize::from(existing));
        assert!(write_config(&dir.0, b"not a file").is_err());
        assert!(dir.0.is_dir());
    }
}

#[test]
fn hard_links_are_rejected_before_staging_and_rechecked_before_commit() {
    let dir = Directory::new();
    let path = dir.0.join("config");
    let alias = dir.0.join("alias");
    fs::write(&path, b"original").unwrap();
    #[cfg(unix)]
    let staged = Replacement::prepare(&path, b"new").unwrap();
    fs::hard_link(&path, &alias).unwrap();
    #[cfg(unix)]
    assert!(staged
        .commit()
        .unwrap_err()
        .to_string()
        .contains("multiple hard links"));
    for candidate in [&path, &alias] {
        let error = write_config(candidate, b"new").unwrap_err().to_string();
        assert!(error.contains(&candidate.display().to_string()));
        assert_eq!(fs::read(candidate).unwrap(), b"original");
        assert_eq!(
            crate::platform::config_file_link_count(candidate).unwrap(),
            2
        );
    }
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 2);
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}
#[cfg(windows)]
fn symlink(target: &Path, link: &Path) {
    std::os::windows::fs::symlink_file(target, link).unwrap();
}

#[test]
fn symlink_chains_and_dangling_targets_preserve_links() {
    let dir = Directory::new();
    let other = dir.0.join("other");
    fs::create_dir(&other).unwrap();
    let target = other.join("preferences");
    let intermediate = other.join("link");
    let entry = dir.0.join("config");
    symlink(&target, &intermediate);
    // A Windows reparse target needs native separators, unlike ordinary Win32 paths.
    let relative_target = Path::new("other").join("link");
    symlink(&relative_target, &entry);
    let original_intermediate = fs::read_link(&intermediate).unwrap();
    assert_eq!(
        fs::metadata(&entry).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    write_config(&entry, b"first install").unwrap();
    write_config(&entry, b"second install").unwrap();
    assert_eq!(fs::read(&target).unwrap(), b"second install");
    assert_eq!(fs::read_link(&entry).unwrap(), relative_target);
    assert_eq!(fs::read_link(&intermediate).unwrap(), original_intermediate);
    assert_eq!(fs::read_dir(&other).unwrap().count(), 2);
    let alias = other.join("hard-link");
    fs::hard_link(&target, &alias).unwrap();
    assert!(write_config(&entry, b"must not change").is_err());
    assert_eq!(fs::read(&alias).unwrap(), b"second install");
    assert_eq!(fs::read_link(&entry).unwrap(), relative_target);

    let cycle = dir.0.join("cycle");
    symlink(Path::new("cycle"), &cycle);
    assert!(write_config(&cycle, b"must not replace the link").is_err());
    assert_eq!(fs::read_link(&cycle).unwrap(), Path::new("cycle"));
}

#[test]
fn existing_permissions_and_new_file_defaults_are_preserved() {
    let dir = Directory::new();
    let path = dir.0.join("existing");
    fs::write(&path, b"original").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
    }
    let permissions = fs::metadata(&path).unwrap().permissions();
    write_config(&path, b"new").unwrap();
    assert_eq!(fs::metadata(&path).unwrap().permissions(), permissions);
    let ordinary = dir.0.join("ordinary");
    let new = dir.0.join("new");
    fs::write(&ordinary, b"ordinary creation").unwrap();
    write_config(&new, b"atomic creation").unwrap();
    assert_eq!(
        fs::metadata(&ordinary).unwrap().permissions(),
        fs::metadata(&new).unwrap().permissions()
    );
}

#[cfg(unix)]
#[test]
fn writable_directory_does_not_bypass_read_only_config() {
    use std::os::unix::{fs::PermissionsExt, process::CommandExt};
    const CHILD: &str = "HERDR_CONFIG_READ_ONLY_TEST";
    if let Some(path) = std::env::var_os(CHILD) {
        let error = write_config(Path::new(&path), b"must not replace").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        println!("read-only rejection executed");
        return;
    }
    let dir = Directory::new();
    let path = dir.0.join("read-only");
    fs::write(&path, b"original").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();
    fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o777)).unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap());
    child
        .args([
            "--exact",
            "integration::config_file::tests::writable_directory_does_not_bypass_read_only_config",
            "--nocapture",
        ])
        .env(CHILD, &path);
    // Root bypasses Unix mode checks. Test the real user path in a child instead.
    if unsafe { libc::geteuid() } == 0 {
        child.gid(65534).uid(65534);
    }
    let output = child.output().unwrap();
    assert!(output.status.success(), "child failed: {output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("read-only rejection executed"));
    assert_eq!(fs::read(&path).unwrap(), b"original");
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
}

#[cfg(unix)]
#[test]
fn partial_write_errors_preserve_files_and_do_not_remove_collisions() {
    const CHILD: &str = "HERDR_CONFIG_PARTIAL_WRITE_TEST";
    if let Some(path) = std::env::var_os(CHILD) {
        let dir = PathBuf::from(path);
        // This process runs only this test. A collision must neither be used nor removed.
        NEXT_TEMP.store(0, Ordering::Relaxed);
        let collision = dir.join(format!(".herdr-config-{}-0.tmp", std::process::id()));
        fs::write(&collision, b"unrelated file").unwrap();
        for name in ["existing", "new"] {
            let error = write_config(&dir.join(name), vec![b'x'; 8192]).unwrap_err();
            assert_eq!(error.raw_os_error(), Some(libc::EFBIG));
        }
        assert_eq!(fs::read(collision).unwrap(), b"unrelated file");
        println!("partial-write paths executed");
        return;
    }
    let dir = Directory::new();
    fs::write(dir.0.join("existing"), b"original").unwrap();
    let output = std::process::Command::new("bash")
        .args(["-c", "trap '' XFSZ; ulimit -f 1; exec \"$@\"", "herdr-test"])
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", "integration::config_file::tests::partial_write_errors_preserve_files_and_do_not_remove_collisions", "--nocapture"])
        .env(CHILD, &dir.0)
        .output().unwrap();
    assert!(output.status.success(), "child failed: {output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("partial-write paths executed"));
    assert_eq!(fs::read(dir.0.join("existing")).unwrap(), b"original");
    assert!(!dir.0.join("new").exists());
    assert_eq!(
        fs::read_dir(&dir.0).unwrap().count(),
        2,
        "only original and collision may remain"
    );
}
