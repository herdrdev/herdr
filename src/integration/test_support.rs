//! Shared helpers for integration tests.

use std::path::Path;

/// Windows refuses file symlinks with `ERROR_PRIVILEGE_NOT_HELD` when the
/// process is neither elevated nor running with Developer Mode enabled.
#[cfg(windows)]
const ERROR_PRIVILEGE_NOT_HELD: i32 = 1314;

#[cfg(windows)]
fn symlink_privilege_denied(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(ERROR_PRIVILEGE_NOT_HELD)
}

/// Create a file symlink, returning `false` when Windows denied the symlink
/// privilege. Callers skip symlink-only assertions on `false` instead of
/// failing an ordinary non-elevated local test run.
#[cfg(unix)]
pub(super) fn symlink_file(target: &Path, link: &Path) -> bool {
    std::os::unix::fs::symlink(target, link).expect("create symlink");
    true
}

#[cfg(windows)]
pub(super) fn symlink_file(target: &Path, link: &Path) -> bool {
    match std::os::windows::fs::symlink_file(target, link) {
        Ok(()) => true,
        Err(error) if symlink_privilege_denied(&error) => {
            eprintln!("skipping symlink test: Windows denied SeCreateSymbolicLinkPrivilege");
            false
        }
        Err(error) => panic!("create symlink {}: {error}", link.display()),
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn symlink_privilege_error_is_detected() {
        assert!(symlink_privilege_denied(
            &std::io::Error::from_raw_os_error(ERROR_PRIVILEGE_NOT_HELD)
        ));
        assert!(!symlink_privilege_denied(
            &std::io::Error::from_raw_os_error(/* ERROR_ACCESS_DENIED */ 5)
        ));
    }
}
