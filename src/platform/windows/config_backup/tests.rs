use super::super::{config_security_descriptor, config_security_sddl};
use super::*;
use std::process::Command;
use windows_sys::Win32::{
    Security::{
        DACL_SECURITY_INFORMATION, GROUP_SECURITY_INFORMATION, LABEL_SECURITY_INFORMATION,
        OWNER_SECURITY_INFORMATION,
    },
    Storage::FileSystem::{
        EncryptFileW, LockFileEx, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
    },
    System::IO::OVERLAPPED,
};

struct Directory(PathBuf);
impl Directory {
    fn new(case: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("herdr-config-backup-{case}-{}", std::process::id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn powershell(script: &str, path: &Path) {
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .env("HERDR_TEST_CONFIG_SOURCE", path)
        .output()
        .unwrap();
    assert!(
        output.status.success() && output.stderr.is_empty(),
        "{output:?}"
    );
}
fn security(path: &Path) -> Vec<u16> {
    let flags = OWNER_SECURITY_INFORMATION
        | GROUP_SECURITY_INFORMATION
        | DACL_SECURITY_INFORMATION
        | LABEL_SECURITY_INFORMATION;
    let mut descriptor = config_security_descriptor(path, flags).unwrap();
    config_security_sddl(&mut descriptor, flags).unwrap()
}
fn identity(path: &Path) -> (u32, u32, u32) {
    let file = File::open(path).unwrap();
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    assert_ne!(
        unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) },
        0
    );
    (
        info.dwVolumeSerialNumber,
        info.nFileIndexHigh,
        info.nFileIndexLow,
    )
}
fn lock_range(path: &Path, offset: u32) -> File {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    let mut overlapped = OVERLAPPED::default();
    overlapped.Anonymous.Anonymous.Offset = offset;
    assert_ne!(
        unsafe {
            LockFileEx(
                file.as_raw_handle(),
                LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                0,
                1,
                0,
                &mut overlapped,
            )
        },
        0,
        "{}",
        io::Error::last_os_error()
    );
    file
}
fn successful_update(case: &str) {
    let dir = Directory::new(case);
    let source = dir.0.join("config");
    fs::write(&source, b"original preferences").unwrap();
    fs::write(dir.0.join("config:private"), b"original stream").unwrap();
    if case == "protected" || case == "unprotected" || case == "moved" {
        powershell(
            &format!(
                r#"
$ErrorActionPreference = 'Stop'
$acl = [System.IO.File]::GetAccessControl($env:HERDR_TEST_CONFIG_SOURCE)
$acl.SetAccessRuleProtection(${}, $false)
$sid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User
$rule = [System.Security.AccessControl.FileSystemAccessRule]::new($sid, [System.Security.AccessControl.FileSystemRights]::FullControl, [System.Security.AccessControl.AccessControlType]::Allow)
$acl.AddAccessRule($rule)
[System.IO.File]::SetAccessControl($env:HERDR_TEST_CONFIG_SOURCE, $acl)
"#,
                case == "protected"
            ),
            &source,
        );
    }
    if case == "metadata" {
        powershell(
            r#"
$ErrorActionPreference = 'Stop'
$acl = [System.IO.File]::GetAccessControl($env:HERDR_TEST_CONFIG_SOURCE)
$acl.SetOwner([System.Security.Principal.WindowsIdentity]::GetCurrent().User)
$acl.SetGroup([System.Security.Principal.SecurityIdentifier]::new('S-1-5-32-545'))
[System.IO.File]::SetAccessControl($env:HERDR_TEST_CONFIG_SOURCE, $acl)
$null = & icacls.exe $env:HERDR_TEST_CONFIG_SOURCE /setintegritylevel L
if ($LASTEXITCODE -ne 0) { throw 'could not install low integrity label' }
"#,
            &source,
        );
    }
    let source = if case == "moved" {
        let original = security(&source);
        let parent = dir.0.join("broader-parent");
        fs::create_dir(&parent).unwrap();
        powershell(
            r#"
$ErrorActionPreference = 'Stop'
$acl = [System.IO.Directory]::GetAccessControl($env:HERDR_TEST_CONFIG_SOURCE)
$sid = [System.Security.Principal.SecurityIdentifier]::new('S-1-5-32-546')
$rule = [System.Security.AccessControl.FileSystemAccessRule]::new($sid, [System.Security.AccessControl.FileSystemRights]::Read, [System.Security.AccessControl.InheritanceFlags]::ObjectInherit, [System.Security.AccessControl.PropagationFlags]::None, [System.Security.AccessControl.AccessControlType]::Allow)
$acl.AddAccessRule($rule)
[System.IO.Directory]::SetAccessControl($env:HERDR_TEST_CONFIG_SOURCE, $acl)
"#,
            &parent,
        );
        let moved = parent.join("config");
        fs::rename(&source, &moved).unwrap();
        assert_eq!(security(&moved), original);
        assert!(!String::from_utf16_lossy(&original).contains(";;;BG)"));
        let inherited = parent.join("inherited");
        fs::write(&inherited, b"").unwrap();
        assert!(String::from_utf16_lossy(&security(&inherited)).contains(";;;BG)"));
        moved
    } else {
        source
    };
    let before = security(&source);
    let before_id = identity(&source);
    let text = String::from_utf16_lossy(&before);
    match case {
        "legacy" => assert!(text.contains("D:("), "{text}"),
        "protected" => assert!(text.contains("D:PAI"), "{text}"),
        "unprotected" | "moved" => assert!(text.contains("D:AI"), "{text}"),
        "metadata" => assert!(text.contains("G:BU") && text.contains(";;;LW)"), "{text}"),
        _ => unreachable!(),
    }
    let (complete, pending) = backup_paths(&source);
    let mut phases = Vec::new();
    assert!(write_observed(&source, b"new", |phase| {
        phases.push(phase);
        if phase == Phase::BackupReady {
            assert_eq!(fs::read(&complete).unwrap(), b"original preferences");
            assert_eq!(fs::read(&source).unwrap(), b"original preferences");
            assert!(!pending.exists());
            let private = String::from_utf16_lossy(&security(&complete));
            assert!(
                private.contains("D:P") && !private.contains(";;;BG)"),
                "{private}"
            );
        }
        if phase == Phase::Truncated {
            assert!(fs::read(&source).unwrap().is_empty());
        }
        Ok(())
    })
    .unwrap());
    assert_eq!(
        phases,
        [
            Phase::BackupChunk,
            Phase::BackupReady,
            Phase::Truncated,
            Phase::Committed
        ]
    );
    assert_eq!(fs::read(&source).unwrap(), b"new");
    assert_eq!(
        fs::read(source.with_file_name("config:private")).unwrap(),
        b"original stream"
    );
    assert_eq!(security(&source), before);
    assert_eq!(identity(&source), before_id);
    assert!(!complete.exists() && !pending.exists());
}
#[test]
fn backup_preserves_legacy() {
    successful_update("legacy");
}
#[test]
fn backup_preserves_protected() {
    successful_update("protected");
}
#[test]
fn backup_preserves_unprotected() {
    successful_update("unprotected");
}
#[test]
fn backup_preserves_moved() {
    successful_update("moved");
}
#[test]
fn backup_preserves_metadata() {
    successful_update("metadata");
}

#[test]
fn native_io_failures_preserve_recovery_ordering() {
    let dir = Directory::new("io-failures");
    for during_backup in [true, false] {
        let path = dir.0.join(if during_backup { "backup" } else { "write" });
        let original = vec![b'a'; if during_backup { 32768 } else { 16 }];
        fs::write(&path, &original).unwrap();
        let before = security(&path);
        let before_id = identity(&path);
        // First case fails a later backup read after one real chunk. In the
        // second, lock after truncation so the actual new WriteFile must fail.
        let mut locked = during_backup.then(|| lock_range(&path, 8192));
        let mut phases = Vec::new();
        let error = write_observed(&path, &vec![b'x'; 16384], |phase| {
            phases.push(phase);
            if !during_backup && phase == Phase::Truncated {
                locked = Some(lock_range(&path, 4096));
            }
            Ok(())
        })
        .unwrap_err();
        eprintln!("during_backup={during_backup}: {error}; phases={phases:?}");
        assert!(error.to_string().contains("os error 33"));
        drop(locked);
        let (complete, pending) = backup_paths(&path);
        assert!(!pending.exists());
        assert_eq!(security(&path), before);
        assert_eq!(identity(&path), before_id);
        if during_backup {
            assert_eq!(phases, [Phase::BackupChunk]);
            assert_eq!(fs::read(&path).unwrap(), original);
            assert!(!complete.exists());
        } else {
            assert!(phases.contains(&Phase::Truncated));
            assert_eq!(fs::read(&complete).unwrap(), original);
            assert!(error.to_string().contains(&complete.display().to_string()));
            assert!(write_existing(&path, b"retry")
                .unwrap_err()
                .to_string()
                .contains("recovery copy"));
            // Recovery is contents-only, never rename over the original file.
            fs::write(&path, fs::read(&complete).unwrap()).unwrap();
            assert_eq!(identity(&path), before_id);
            assert_eq!(security(&path), before);
            fs::remove_file(&complete).unwrap();
            assert!(write_existing(&path, b"resolved").unwrap());
        }
    }
}

#[test]
fn interrupted_backups_and_writes_are_distinguishable() {
    const CHILD: &str = "HERDR_BACKUP_CRASH_TEST";
    if let Some(path) = std::env::var_os(CHILD) {
        let path = PathBuf::from(path);
        let crash_phase = if path.file_name().unwrap() == "copy" {
            Phase::BackupChunk
        } else {
            Phase::Truncated
        };
        let _ = write_observed(&path, b"new", |phase| {
            if phase == crash_phase {
                std::process::exit(23);
            }
            Ok(())
        });
        panic!("crash phase not reached");
    }
    let dir = Directory::new("crash");
    for name in ["copy", "write"] {
        let path = dir.0.join(name);
        let original = vec![b'a'; 32768];
        fs::write(&path, &original).unwrap();
        let output = Command::new(std::env::current_exe().unwrap()).args(["--exact", "platform::windows::config_backup::tests::interrupted_backups_and_writes_are_distinguishable", "--nocapture"])
            .env(CHILD, &path).output().unwrap();
        assert_eq!(output.status.code(), Some(23), "{output:?}");
        let (complete, pending) = backup_paths(&path);
        if name == "copy" {
            assert_eq!(fs::read(&pending).unwrap().len(), 8192);
            assert!(!complete.exists());
            assert_eq!(fs::read(&path).unwrap(), original);
            assert!(check_recovery(&path)
                .unwrap_err()
                .to_string()
                .contains("unfinished backup"));
        } else {
            assert!(!pending.exists());
            assert_eq!(fs::read(&complete).unwrap(), original);
            assert!(fs::read(&path).unwrap().is_empty());
            fs::remove_file(&path).unwrap();
            assert!(write_existing(&path, b"must not recreate").is_err());
            assert!(!path.exists());
            assert_eq!(fs::read(&complete).unwrap(), original);
        }
    }
}

#[test]
fn completed_write_cleanup_failure_and_foreign_backups_are_reported() {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};
    let dir = Directory::new("cleanup");
    let path = dir.0.join("config");
    fs::write(&path, b"original").unwrap();
    let (complete, _) = backup_paths(&path);
    fs::write(&complete, b"foreign contents").unwrap();
    assert!(write_existing(&path, b"rejected").is_err());
    assert_eq!(fs::read(&complete).unwrap(), b"foreign contents");
    assert_eq!(fs::read(&path).unwrap(), b"original");
    fs::remove_file(&complete).unwrap();
    let mut held = None;
    let error = write_observed(&path, b"saved", |phase| {
        if phase == Phase::Committed {
            held = Some(
                OpenOptions::new()
                    .read(true)
                    .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                    .open(&complete)?,
            );
        }
        Ok(())
    })
    .unwrap_err();
    assert!(
        error.to_string().contains("updated ")
            && error.to_string().contains("recovery copy remains")
    );
    assert_eq!(fs::read(&path).unwrap(), b"saved");
    assert_eq!(fs::read(&complete).unwrap(), b"original");
    assert!(write_existing(&path, b"retry").is_err());
    drop(held);
}

#[test]
fn encrypted_config_is_rejected_without_plaintext_backup() {
    let dir = Directory::new("encrypted");
    let path = dir.0.join("config");
    fs::write(&path, b"encrypted original").unwrap();
    let wide = super::super::extended_length_path(&path).unwrap();
    assert_ne!(
        unsafe { EncryptFileW(wide.as_ptr()) },
        0,
        "cannot establish EFS fixture: {}",
        io::Error::last_os_error()
    );
    assert_ne!(
        fs::metadata(&path).unwrap().file_attributes() & FILE_ATTRIBUTE_ENCRYPTED,
        0
    );
    let before = security(&path);
    let before_id = identity(&path);
    let error = write_existing(&path, b"new").unwrap_err();
    assert!(error.to_string().contains("encrypted configs"));
    assert_eq!(fs::read(&path).unwrap(), b"encrypted original");
    assert_eq!(security(&path), before);
    assert_eq!(identity(&path), before_id);
    let (complete, pending) = backup_paths(&path);
    assert!(!complete.exists() && !pending.exists());
}
