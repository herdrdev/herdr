//! Unix-owned Kitty temporary-file ledger and bounded crash recovery.

use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

const MAX_FILES: usize = 8;
const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_IMAGE_BYTES: usize = 16 * 1024 * 1024;
// Bound even abandoned generations: at most 512 MiB per Unix user. When all
// slots are live/recent, a new client uses inline rather than allocating more.
const MAX_GENERATIONS: usize = 8;
const STALE_GRACE: Duration = Duration::from_secs(60);
const GENERATION_PREFIX: &str = "herdr-tty-graphics-protocol-";
const CONSUMPTION_TIMEOUT: Duration = Duration::from_secs(10);

struct Pending {
    path: PathBuf,
    bytes: usize,
    published: Instant,
}

enum ProbeState {
    Unsent,
    Pending(PathBuf),
    Consumed,
}

pub(crate) struct Ledger {
    root: PathBuf,
    directory: Option<Generation>,
    pending: Vec<Pending>,
    next: u64,
    disabled: bool,
    probe: ProbeState,
}

impl Ledger {
    #[cfg(test)]
    pub(crate) fn for_test(root: PathBuf) -> Self {
        let mut ledger = Self::new();
        ledger.root = root;
        ledger
    }

    pub(crate) fn new() -> Self {
        Self {
            // Do not use TMPDIR/XDG_RUNTIME_DIR: a client override need not
            // be in the terminal's accepted temporary-file roots. /tmp is
            // explicitly accepted by Ghostty graphics_image.zig in v1.1.3,
            // v1.2.3 and main, independent of its TMPDIR. /var/tmp is NOT
            // generally accepted, so this bounded ledger must use /tmp.
            root: PathBuf::from("/tmp"),
            directory: None,
            pending: Vec::new(),
            next: 0,
            disabled: false,
            probe: ProbeState::Unsent,
        }
    }

    pub(crate) fn probe(&mut self) -> Option<PathBuf> {
        if self.disabled || !matches!(self.probe, ProbeState::Unsent) {
            return None;
        }
        let path = self.prepare_file_at(&[0, 0, 0, 0], Instant::now())?;
        self.probe = ProbeState::Pending(path.clone());
        Some(path)
    }

    pub(crate) fn prepare(&mut self, data: &[u8]) -> Option<PathBuf> {
        self.prepare_at(data, Instant::now())
    }

    fn prepare_at(&mut self, data: &[u8], now: Instant) -> Option<PathBuf> {
        self.reap(now);
        if !matches!(self.probe, ProbeState::Consumed) {
            return None;
        }
        self.prepare_file_at(data, now)
    }

    fn prepare_file_at(&mut self, data: &[u8], now: Instant) -> Option<PathBuf> {
        let bytes: usize = self.pending.iter().map(|entry| entry.bytes).sum();
        if self.disabled
            || data.is_empty()
            || data.len() > MAX_IMAGE_BYTES
            || self.pending.len() >= MAX_FILES
            || data.len() > MAX_BYTES - bytes
        {
            return None;
        }
        match self.write_file(data) {
            Ok(path) => {
                self.pending.push(Pending {
                    path: path.clone(),
                    bytes: data.len(),
                    published: now,
                });
                Some(path)
            }
            Err(_) => {
                // Do not repeatedly perform failing I/O on every frame.
                self.disabled = true;
                None
            }
        }
    }

    fn reap(&mut self, now: Instant) {
        if let ProbeState::Pending(path) = &self.probe {
            if matches!(fs::symlink_metadata(path), Err(error) if error.kind() == io::ErrorKind::NotFound)
            {
                // With the allowlisted raw query this demonstrates local medium
                // consumption, not arbitrary image acceptance or presentation.
                self.probe = ProbeState::Consumed;
            }
        }
        self.pending
            .retain(|entry| match fs::symlink_metadata(&entry.path) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => false,
                _ => true, // Permission/I/O errors are NOT evidence of consumption.
            });
        if self
            .pending
            .iter()
            .any(|entry| now.saturating_duration_since(entry.published) >= CONSUMPTION_TIMEOUT)
        {
            self.disabled = true;
        }
    }

    fn write_file(&mut self, data: &[u8]) -> io::Result<PathBuf> {
        if self.directory.is_none() {
            self.directory = Some(create_generation(&self.root, SystemTime::now())?);
        }
        let number = self.next;
        self.next = self
            .next
            .checked_add(1)
            .ok_or_else(|| io::Error::other("file sequence exhausted"))?;
        let path = self
            .directory
            .as_ref()
            .ok_or_else(|| io::Error::other("missing file generation"))?
            .path
            .join(format!("tty-graphics-protocol-{number}"));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        let result = file
            .set_permissions(fs::Permissions::from_mode(0o600))
            .and_then(|()| file.write_all(data));
        // Close before publishing, including on write errors. No fsync is
        // needed: the terminal reads the same filesystem/page cache.
        drop(file);
        if let Err(error) = result {
            if fs::remove_file(&path).is_err() {
                // Never published, but retain ownership if immediate
                // cleanup failed so shutdown can retry the exact path.
                self.pending.push(Pending {
                    path,
                    bytes: data.len(),
                    published: Instant::now(),
                });
            }
            return Err(error);
        }
        Ok(path)
    }

    pub(crate) fn cleanup(&mut self) {
        self.disabled = true;
        for entry in self.pending.drain(..) {
            let _ = fs::remove_file(entry.path);
        }
        if let Some(directory) = self.directory.take() {
            // Never recursively delete: remove only paths we created.
            let _ = fs::remove_dir(&directory.path);
        }
    }
}

impl Drop for Ledger {
    fn drop(&mut self) {
        self.cleanup();
    }
}

/// Keeping this directory descriptor locked prevents recovery by another client.
struct Generation {
    path: PathBuf,
    _lock: fs::File,
}

fn effective_uid() -> u32 {
    // SAFETY: geteuid takes no pointers and has no preconditions.
    unsafe { libc::geteuid() }
}

fn open_private_directory(path: &Path) -> io::Result<fs::File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let metadata = file.metadata()?;
    if metadata.uid() != effective_uid() || metadata.mode() & 0o7777 != 0o700 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "untrusted image directory",
        ));
    }
    Ok(file)
}

fn lock_directory(file: &fs::File) -> io::Result<()> {
    // SAFETY: this live File owns the descriptor; flock neither consumes it nor
    // dereferences pointers. Drop releases the advisory lock automatically.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn create_generation(root: &Path, now: SystemTime) -> io::Result<Generation> {
    let parent = root.join(format!("herdr-kitty-files-{}", effective_uid()));
    match DirBuilder::new().mode(0o700).create(&parent) {
        Ok(()) => fs::set_permissions(&parent, fs::Permissions::from_mode(0o700))?,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    // The persistent per-user parent is never removed: replacing its inode
    // would split its lock domain. Other users cannot populate or rename it.
    let parent_lock = open_private_directory(&parent)?;
    lock_directory(&parent_lock)?;
    if recover_generations(&parent, now)? >= MAX_GENERATIONS {
        return Err(io::Error::other("image generation budget exhausted"));
    }
    let path = private_directory(&parent)?;
    let result = open_private_directory(&path).and_then(|file| {
        lock_directory(&file)?;
        Ok(file)
    });
    match result {
        Ok(file) => Ok(Generation { path, _lock: file }),
        Err(error) => {
            let _ = fs::remove_dir(path);
            Err(error)
        }
    }
}

/// Caller holds the parent lock. Only unlocked, old generations can be removed;
/// terminal reads belonging to live clients are never invalidated by recovery.
/// Crash recovery is opportunistic at the next client's first file preparation.
fn recover_generations(parent: &Path, now: SystemTime) -> io::Result<usize> {
    let entries = fs::read_dir(parent)?
        .take(MAX_GENERATIONS + 1)
        .collect::<io::Result<Vec<_>>>()?;
    if entries.len() > MAX_GENERATIONS {
        return Err(io::Error::other("unexpected image generation count"));
    }
    let mut remaining = entries.len();
    for entry in entries {
        let name = entry.file_name();
        let Some(suffix) = name
            .to_str()
            .and_then(|name| name.strip_prefix(GENERATION_PREFIX))
        else {
            continue;
        };
        if suffix.len() != 32 || !suffix.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        let path = entry.path();
        let Ok(lock) = open_private_directory(&path) else {
            continue;
        };
        if lock_directory(&lock).is_err() {
            continue;
        }
        let metadata = lock.metadata()?;
        let old = metadata
            .modified()
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age >= STALE_GRACE);
        if old && remove_stale_generation(&path)? {
            remaining -= 1;
        }
    }
    Ok(remaining)
}

/// Inspect the entire bounded set before deleting anything. Never follow links,
/// recurse, or remove unknown content, even inside our own namespace.
fn remove_stale_generation(path: &Path) -> io::Result<bool> {
    let entries = fs::read_dir(path)?
        .take(MAX_FILES + 1)
        .collect::<io::Result<Vec<_>>>()?;
    if entries.len() > MAX_FILES {
        return Ok(false);
    }
    for entry in &entries {
        let name = entry.file_name();
        let valid_name = name
            .to_str()
            .and_then(|name| name.strip_prefix("tty-graphics-protocol-"))
            .is_some_and(|suffix| !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()));
        let metadata = fs::symlink_metadata(entry.path())?;
        if !valid_name
            || !metadata.is_file()
            || metadata.uid() != effective_uid()
            || metadata.mode() & 0o7777 != 0o600
            || metadata.nlink() != 1
        {
            return Ok(false);
        }
    }
    for entry in entries {
        fs::remove_file(entry.path())?;
    }
    fs::remove_dir(path)?;
    Ok(true)
}

fn private_directory(root: &Path) -> io::Result<PathBuf> {
    for _ in 0..8 {
        let mut random = [0u8; 16];
        fs::File::open("/dev/urandom")?.read_exact(&mut random)?;
        let name = u128::from_ne_bytes(random);
        let path = root.join(format!("{GENERATION_PREFIX}{name:032x}"));
        match DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => {
                if let Err(error) = fs::set_permissions(&path, fs::Permissions::from_mode(0o700)) {
                    let _ = fs::remove_dir(&path);
                    return Err(error);
                }
                return Ok(path);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "temporary directory collisions",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestLedger {
        ledger: Ledger,
        scratch: PathBuf,
    }

    impl std::ops::Deref for TestLedger {
        type Target = Ledger;
        fn deref(&self) -> &Ledger {
            &self.ledger
        }
    }

    impl std::ops::DerefMut for TestLedger {
        fn deref_mut(&mut self) -> &mut Ledger {
            &mut self.ledger
        }
    }

    impl Drop for TestLedger {
        fn drop(&mut self) {
            self.ledger.cleanup();
            // Only this test's uniquely allocated private scratch tree.
            let _ = fs::remove_dir_all(&self.scratch);
        }
    }

    fn test_ledger() -> TestLedger {
        let scratch = private_directory(Path::new("/var/tmp")).unwrap();
        let mut ledger = Ledger::new();
        ledger.root = scratch.clone();
        // Most ledger tests start after successful query consumption.
        ledger.probe = ProbeState::Consumed;
        TestLedger { ledger, scratch }
    }

    #[test]
    fn private_complete_unique_files_and_shutdown() {
        let mut ledger = test_ledger();
        let first = ledger.prepare(b"raw\0pixels").unwrap();
        let second = ledger.prepare(b"other").unwrap();
        let dir = first.parent().unwrap().to_owned();
        assert_ne!(first, second);
        assert_eq!(fs::read(&first).unwrap(), b"raw\0pixels");
        assert_eq!(
            fs::metadata(&first).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert!(first
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .contains("tty-graphics-protocol"));
        let mut other = test_ledger();
        assert_ne!(
            other.prepare(b"other client").unwrap().parent().unwrap(),
            dir
        );
        drop(ledger);
        assert!(!first.exists());
        assert!(!second.exists());
        assert!(!dir.exists());
    }

    #[test]
    fn count_cap_and_terminal_consumption() {
        let mut ledger = test_ledger();
        for _ in 0..MAX_FILES {
            assert!(ledger.prepare(b"x").is_some());
        }
        assert!(ledger.prepare(b"inline").is_none());
        let consumed = ledger.pending[0].path.clone();
        fs::remove_file(consumed).unwrap();
        assert!(ledger.prepare(b"replacement").is_some());
        assert_eq!(ledger.pending.len(), MAX_FILES);
    }

    #[test]
    fn byte_and_image_caps() {
        let mut ledger = test_ledger();
        assert!(ledger.prepare(&[]).is_none());
        let mut data = vec![0; MAX_IMAGE_BYTES + 1];
        assert!(ledger.prepare(&data).is_none());
        assert!(ledger.directory.is_none());
        data.pop();
        for _ in 0..MAX_BYTES / MAX_IMAGE_BYTES {
            assert!(ledger.prepare(&data).is_some());
        }
        assert!(ledger.prepare(b"x").is_none());
        fs::remove_file(&ledger.pending[0].path).unwrap();
        assert!(ledger.prepare(&data).is_some());
    }

    #[test]
    fn timeout_disables_without_unlinking_published_files() {
        let mut ledger = test_ledger();
        let start = Instant::now();
        let path = ledger.prepare_at(b"pending", start).unwrap();
        assert!(ledger
            .prepare_at(b"inline", start + CONSUMPTION_TIMEOUT)
            .is_none());
        assert_eq!(fs::read(&path).unwrap(), b"pending");
        fs::remove_file(path).unwrap();
        assert!(ledger
            .prepare_at(b"still inline", start + CONSUMPTION_TIMEOUT)
            .is_none());
    }

    #[test]
    fn local_creation_errors_fall_back_and_never_overwrite() {
        let mut ledger = test_ledger();
        let path = ledger.prepare(b"published").unwrap();
        ledger.next = 0; // Force an exclusive-create collision.
        assert!(ledger.prepare(b"must not overwrite").is_none());
        assert_eq!(fs::read(path).unwrap(), b"published");
        let mut broken = test_ledger();
        broken.root = ledger.pending[0].path.clone(); // Not a directory.
        assert!(broken.prepare(b"inline").is_none());
        assert!(broken.disabled);
    }

    #[test]
    fn probe_is_once_bounded_and_required_before_real_uploads() {
        let mut ledger = test_ledger();
        ledger.probe = ProbeState::Unsent;
        assert!(ledger.prepare(b"real").is_none());
        assert!(ledger.directory.is_none());
        let query = ledger.probe().unwrap();
        assert_eq!(fs::read(&query).unwrap(), [0, 0, 0, 0]);
        assert_eq!(ledger.pending.len(), 1);
        assert_eq!(ledger.pending[0].bytes, 4);
        assert!(ledger.probe().is_none());
        assert!(ledger.prepare(b"real").is_none());
        fs::remove_file(query).unwrap();
        assert!(ledger.prepare(b"real").is_some());
        assert!(ledger.probe().is_none());
    }

    #[test]
    fn probe_timeout_preserves_file_and_permanently_disables_conversion() {
        let mut ledger = test_ledger();
        ledger.probe = ProbeState::Unsent;
        let query = ledger.probe().unwrap();
        let late = ledger.pending[0].published + CONSUMPTION_TIMEOUT;
        assert!(ledger.prepare_at(b"real", late).is_none());
        assert!(ledger.disabled);
        assert_eq!(fs::read(&query).unwrap(), [0, 0, 0, 0]);
        fs::remove_file(query).unwrap();
        assert!(ledger.prepare_at(b"real", late).is_none());
        assert!(ledger.probe().is_none());
    }

    fn recover_for_test(parent: &Path, now: SystemTime) -> usize {
        let lock = open_private_directory(parent).unwrap();
        lock_directory(&lock).unwrap();
        recover_generations(parent, now).unwrap()
    }

    #[test]
    fn recovery_preserves_live_and_recent_but_removes_abandoned_generations() {
        let mut ledger = test_ledger();
        let image = ledger.prepare(b"live").unwrap();
        let generation_path = image.parent().unwrap().to_owned();
        let parent = generation_path.parent().unwrap();
        let later = SystemTime::now() + STALE_GRACE + Duration::from_secs(1);
        assert_eq!(recover_for_test(parent, later), 1);
        assert!(image.exists()); // Advisory lock protects live clients even if old.
        let generation = ledger.directory.take().unwrap();
        ledger.pending.clear(); // Simulate process death without Drop cleanup.
        drop(generation);
        assert_eq!(recover_for_test(parent, SystemTime::now()), 1);
        assert!(image.exists()); // Grace protects terminal work after a crash.
        assert_eq!(recover_for_test(parent, later), 0);
        assert!(!generation_path.exists());
    }

    #[test]
    fn generation_cap_bounds_crash_leftovers_and_parent_lock_is_nonblocking() {
        let fixture = test_ledger();
        let root = &fixture.scratch;
        let now = SystemTime::now();
        let mut generations = Vec::new();
        for _ in 0..MAX_GENERATIONS {
            generations.push(create_generation(root, now).unwrap());
        }
        assert!(create_generation(root, now).is_err());
        let parent = generations[0].path.parent().unwrap().to_owned();
        let lock = open_private_directory(&parent).unwrap();
        lock_directory(&lock).unwrap();
        assert!(create_generation(root, now).is_err());
        drop(lock);
        drop(generations); // Empty abandoned generations, as after an early crash.
        assert!(create_generation(root, now).is_err());
        let recovered =
            create_generation(root, now + STALE_GRACE + Duration::from_secs(1)).unwrap();
        assert_eq!(fs::read_dir(parent).unwrap().count(), 1);
        drop(recovered);
    }

    #[test]
    fn recovery_rejects_symlinks_and_unknown_content() {
        use std::os::unix::fs::symlink;
        let mut ledger = test_ledger();
        let image = ledger.prepare(b"keep").unwrap();
        let generation_path = image.parent().unwrap().to_owned();
        let parent = generation_path.parent().unwrap();
        fs::remove_file(&image).unwrap();
        let outside = ledger.scratch.join("outside");
        fs::write(&outside, b"outside").unwrap();
        symlink(&outside, &image).unwrap();
        ledger.pending.clear();
        drop(ledger.directory.take());
        let later = SystemTime::now() + STALE_GRACE + Duration::from_secs(1);
        assert_eq!(recover_for_test(parent, later), 1);
        assert_eq!(fs::read(&outside).unwrap(), b"outside");
        fs::remove_file(&image).unwrap();
        fs::write(generation_path.join("unknown"), b"keep").unwrap();
        assert_eq!(recover_for_test(parent, later), 1);
        assert!(generation_path.exists());
    }

    #[test]
    fn existing_parent_must_be_owned_private_directory_not_symlink() {
        use std::os::unix::fs::symlink;
        let fixture = test_ledger();
        let parent = fixture
            .scratch
            .join(format!("herdr-kitty-files-{}", effective_uid()));
        let destination = fixture.scratch.join("destination");
        fs::create_dir(&destination).unwrap();
        symlink(&destination, &parent).unwrap();
        assert!(create_generation(&fixture.scratch, SystemTime::now()).is_err());
        fs::remove_file(&parent).unwrap();
        fs::create_dir(&parent).unwrap();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(create_generation(&fixture.scratch, SystemTime::now()).is_err());
    }
}
