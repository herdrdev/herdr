#[cfg(unix)]
use std::fs::OpenOptions;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, FileExt, MetadataExt, OpenOptionsExt, PermissionsExt};

#[cfg(unix)]
const DIRECTORY_MODE: u32 = 0o700;
#[cfg(unix)]
const FILE_MODE: u32 = 0o600;

#[derive(Debug)]
pub(crate) struct FileStore {
    base: PathBuf,
    generation: OnceLock<Arc<Generation>>,
    next_fingerprint: AtomicU64,
    native_budget: Option<Arc<Mutex<NativeBudget>>>,
}

#[derive(Debug)]
struct Generation {
    root: PathBuf,
    source: PathBuf,
}

/// Keep this owned snapshot alive until the terminal has consumed its path.
#[derive(Debug)]
pub(crate) struct OwnedExport {
    lease: Lease,
    _file: ExportFile,
    _reservation: Option<Reservation>,
}

// Also owns cleanup during writing/validation, before an export is published.
#[derive(Debug)]
struct ExportFile {
    path: PathBuf,
    _generation: Arc<Generation>,
}

#[derive(Debug, Default)]
struct NativeBudget {
    bytes: usize,
    objects: usize,
}

#[derive(Debug)]
struct Reservation {
    budget: Arc<Mutex<NativeBudget>>,
    len: usize,
}

impl Reservation {
    fn acquire(budget: &Arc<Mutex<NativeBudget>>, len: usize) -> io::Result<Self> {
        let mut state = budget.lock().unwrap_or_else(|err| err.into_inner());
        if state.objects >= 64 || len > 64 * 1024 * 1024 - state.bytes {
            return Err(io::Error::other("native snapshot budget exhausted"));
        }
        state.objects += 1;
        state.bytes += len;
        Ok(Self {
            budget: Arc::clone(budget),
            len,
        })
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let mut state = self.budget.lock().unwrap_or_else(|err| err.into_inner());
        state.objects -= 1;
        state.bytes -= self.len;
    }
}

const MAX_EXPORT_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug)]
struct Lease {
    path: PathBuf,
    file: File,
    generation: Arc<Generation>,
    metadata: fs::Metadata,
    len: usize,
    fingerprint: u64,
}

impl Default for FileStore {
    fn default() -> Self {
        Self::new(runtime_base())
    }
}

impl FileStore {
    fn new(base: PathBuf) -> Self {
        Self {
            base,
            generation: OnceLock::new(),
            next_fingerprint: AtomicU64::new(1),
            native_budget: None,
        }
    }

    pub(crate) fn native_sources() -> Self {
        let mut store = Self::new(native_base());
        store.native_budget = Some(Arc::new(Mutex::new(NativeBudget::default())));
        store
    }

    /// Kernel-only bounded snapshot; any unsupported clone returns an error.
    /// The caller keeps the borrowed descriptor alive throughout this call.
    pub(crate) fn snapshot(&self, source_fd: i64, expected_len: usize) -> io::Result<OwnedExport> {
        if expected_len == 0 || expected_len > MAX_EXPORT_BYTES || !expected_len.is_multiple_of(4) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid native RGBA length",
            ));
        }
        let budget = self.native_budget.as_ref().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "not a native source store")
        })?;
        let reservation = Reservation::acquire(budget, expected_len)?;
        let generation = self.generation()?;
        let id = self.next_fingerprint.fetch_add(1, Ordering::Relaxed);
        let path = generation.source.join(format!("native-{id}.rgba"));
        let destination = create_export(&path)?;
        let file = ExportFile {
            path,
            _generation: generation,
        };
        crate::platform::clone_native_image_source(source_fd, &destination, expected_len)?;
        drop(destination);
        let lease = self.lease(&file.path, expected_len)?;
        Ok(OwnedExport {
            lease,
            _file: file,
            _reservation: Some(reservation),
        })
    }

    #[cfg(all(test, unix))]
    pub(crate) fn source_directory(&self) -> io::Result<PathBuf> {
        Ok(self.generation()?.source.clone())
    }

    fn lease(&self, path: &Path, expected_len: usize) -> io::Result<Lease> {
        let generation = self.generation()?;
        validate_child(path, &generation.source)?;
        let file = open_no_follow(path)?;
        let metadata = file.metadata()?;
        validate_metadata(&metadata, expected_len)?;
        validate_path_identity(path, &metadata)?;
        let fingerprint = self.next_fingerprint.fetch_add(1, Ordering::Relaxed);
        Ok(Lease {
            path: path.to_owned(),
            file,
            generation,
            metadata,
            len: expected_len,
            fingerprint,
        })
    }

    /// Snapshot decoded bytes into a private, uniquely named file. Aggregate
    /// outstanding-export limits are the caller's responsibility.
    pub(crate) fn export(&self, data: &[u8]) -> io::Result<OwnedExport> {
        if data.len() > MAX_EXPORT_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "decoded export exceeds 16 MiB",
            ));
        }
        let reservation = self
            .native_budget
            .as_ref()
            .map(|budget| Reservation::acquire(budget, data.len()))
            .transpose()?;
        let generation = self.generation()?;
        // Share the lease identity allocator; create_new also protects against
        // collisions with producer files. Never overwrite or remove a collision.
        let id = self.next_fingerprint.fetch_add(1, Ordering::Relaxed);
        let path = generation.source.join(format!("decoded-{id}.rgba"));
        let mut writer = create_export(&path)?;
        let file = ExportFile {
            path,
            _generation: generation,
        };
        writer.write_all(data)?;
        drop(writer);
        let lease = self.lease(&file.path, data.len())?;
        Ok(OwnedExport {
            lease,
            _file: file,
            _reservation: reservation,
        })
    }

    fn generation(&self) -> io::Result<Arc<Generation>> {
        if let Some(generation) = self.generation.get() {
            return Ok(Arc::clone(generation));
        }
        let generation = Arc::new(create_generation(&self.base)?);
        match self.generation.set(Arc::clone(&generation)) {
            Ok(()) => Ok(generation),
            Err(_) => self
                .generation
                .get()
                .cloned()
                .ok_or_else(|| io::Error::other("pane graphics generation was lost")),
        }
    }

    #[cfg(all(test, unix))]
    pub(crate) fn is_initialized(&self) -> bool {
        self.generation.get().is_some()
    }
}

impl PartialEq for OwnedExport {
    fn eq(&self, other: &Self) -> bool {
        self.path() == other.path() && self.fingerprint() == other.fingerprint()
    }
}

impl Eq for OwnedExport {}

impl OwnedExport {
    pub(crate) fn copy_rgba(&self) -> io::Result<Vec<u8>> {
        self.lease.copy_rgba()
    }

    pub(crate) fn read_into(&self, data: &mut [u8]) -> io::Result<()> {
        self.lease.read_into(data)
    }

    pub(crate) fn path(&self) -> &Path {
        self.lease.path()
    }

    pub(crate) fn len(&self) -> usize {
        self.lease.len()
    }

    pub(crate) fn fingerprint(&self) -> u64 {
        self.lease.fingerprint()
    }
}

impl Drop for ExportFile {
    fn drop(&mut self) {
        if let Err(err) = fs::remove_file(&self.path) {
            if err.kind() != io::ErrorKind::NotFound {
                tracing::warn!(path = %self.path.display(), err = %err, "failed to remove decoded graphics export");
            }
        }
    }
}

impl Lease {
    fn path(&self) -> &Path {
        &self.path
    }

    fn len(&self) -> usize {
        self.len
    }

    fn fingerprint(&self) -> u64 {
        self.fingerprint
    }

    fn copy_rgba(&self) -> io::Result<Vec<u8>> {
        let mut data = vec![0; self.len];
        self.read_into(&mut data)?;
        Ok(data)
    }

    fn read_into(&self, data: &mut [u8]) -> io::Result<()> {
        if data.len() != self.len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "incorrect output length",
            ));
        }
        let _keep_generation_alive = &self.generation;
        validate_path_identity(&self.path, &self.metadata)?;
        read_exact_at(&self.file, data)?;
        if has_byte_at(&self.file, self.len as u64)? {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "frame length changed while leased",
            ));
        }
        validate_metadata(&self.file.metadata()?, self.len)?;
        validate_path_identity(&self.path, &self.metadata)?;
        Ok(())
    }
}

impl Drop for Generation {
    fn drop(&mut self) {
        if let Err(err) = fs::remove_dir_all(&self.root) {
            if err.kind() != io::ErrorKind::NotFound {
                tracing::warn!(path = %self.root.display(), err = %err, "failed to remove pane graphics directory");
            }
        }
    }
}

#[cfg(unix)]
pub(crate) fn validate_direct_source(path: &Path, expected_len: usize) -> io::Result<()> {
    validate_source_under(path, expected_len, runtime_base())
}

#[cfg(unix)]
fn validate_source_under(path: &Path, expected_len: usize, base: PathBuf) -> io::Result<()> {
    let source = path.parent().ok_or_else(invalid_path)?;
    let generation = source.parent().ok_or_else(invalid_path)?;
    if !path.is_absolute()
        || path.file_name().is_none()
        || source.file_name().and_then(|name| name.to_str()) != Some("source")
        || generation.parent() != Some(base.as_path())
        || !generation
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("server-"))
    {
        return Err(invalid_path());
    }
    for directory in [base, generation.to_owned(), source.to_owned()] {
        validate_directory(&directory)?;
    }
    let file = open_no_follow(path)?;
    let metadata = file.metadata()?;
    validate_metadata(&metadata, expected_len)?;
    validate_path_identity(path, &metadata)
}

/// Unlike the API validator, accepts only the dedicated native hierarchy.
#[cfg(unix)]
pub(crate) fn validate_native_source(path: &Path, expected_len: usize) -> io::Result<()> {
    if expected_len == 0 || expected_len > MAX_EXPORT_BYTES || !expected_len.is_multiple_of(4) {
        return Err(invalid_path());
    }
    validate_source_under(path, expected_len, native_base())
}

fn native_base() -> PathBuf {
    #[cfg(unix)]
    {
        PathBuf::from(format!("/var/tmp/herdr-native-sources-{}", effective_uid()))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from("native-sources-unavailable")
    }
}

fn create_generation(base: &Path) -> io::Result<Generation> {
    #[cfg(not(unix))]
    {
        let _ = base;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "file-backed pane graphics require Unix",
        ))
    }
    #[cfg(unix)]
    {
        if base == native_base() {
            match fs::DirBuilder::new().mode(DIRECTORY_MODE).create(base) {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
                Err(err) => return Err(err),
            }
        } else {
            fs::create_dir_all(base)?;
            fs::set_permissions(base, fs::Permissions::from_mode(DIRECTORY_MODE))?;
        }
        validate_directory(base)?;
        remove_stale_generations(base);
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let root = base.join(format!("server-{}-{nonce}", std::process::id()));
        fs::create_dir(&root)?;
        fs::set_permissions(&root, fs::Permissions::from_mode(DIRECTORY_MODE))?;
        let source = root.join("source");
        fs::create_dir(&source)?;
        fs::set_permissions(&source, fs::Permissions::from_mode(DIRECTORY_MODE))?;
        validate_directory(&root)?;
        validate_directory(&source)?;
        Ok(Generation { root, source })
    }
}

fn runtime_base() -> PathBuf {
    #[cfg(unix)]
    {
        let root = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| PathBuf::from("/var/tmp"));
        root.join(format!("herdr-pane-graphics-{}", effective_uid()))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from("pane-graphics-unavailable")
    }
}

#[cfg(unix)]
fn read_exact_at(file: &File, data: &mut [u8]) -> io::Result<()> {
    file.read_exact_at(data, 0)
}

#[cfg(not(unix))]
fn read_exact_at(_file: &File, _data: &mut [u8]) -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "Unix only"))
}

#[cfg(unix)]
fn has_byte_at(file: &File, offset: u64) -> io::Result<bool> {
    Ok(file.read_at(&mut [0], offset)? != 0)
}

#[cfg(not(unix))]
fn has_byte_at(_file: &File, _offset: u64) -> io::Result<bool> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "Unix only"))
}

#[cfg(unix)]
fn remove_stale_generations(base: &Path) {
    let Ok(entries) = fs::read_dir(base) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.strip_prefix("server-"))
            .and_then(|name| name.split('-').next())
            .and_then(|pid| pid.parse::<i32>().ok())
            .filter(|pid| *pid > 0)
        else {
            continue;
        };
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        // SAFETY: kill with signal 0 probes process existence without sending a signal.
        let alive = unsafe { libc::kill(pid, 0) } == 0;
        if alive || io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
            continue;
        }
        if let Err(err) = fs::remove_dir_all(entry.path()) {
            tracing::warn!(path = %entry.path().display(), err = %err, "failed to remove stale pane graphics directory");
        }
    }
}

fn validate_child(path: &Path, directory: &Path) -> io::Result<()> {
    if !path.is_absolute() || path.parent() != Some(directory) || path.file_name().is_none() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "frame path must be a direct child of the advertised source directory",
        ));
    }
    Ok(())
}

fn create_export(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(FILE_MODE)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(io::Error::new(io::ErrorKind::Unsupported, "Unix only"))
    }
}

fn open_no_follow(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(io::Error::new(io::ErrorKind::Unsupported, "Unix only"))
    }
}

fn validate_metadata(metadata: &fs::Metadata, expected_len: usize) -> io::Result<()> {
    #[cfg(unix)]
    {
        if !metadata.file_type().is_file()
            || metadata.uid() != effective_uid()
            || metadata.mode() & 0o777 != FILE_MODE
            || metadata.nlink() != 1
            || metadata.len() != expected_len as u64
        {
            return Err(invalid(
                "frame must be same-uid regular 0600 single-link exact-length file",
            ));
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (metadata, expected_len);
        Err(io::Error::new(io::ErrorKind::Unsupported, "Unix only"))
    }
}

#[cfg(unix)]
fn validate_directory(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir()
        || metadata.uid() != effective_uid()
        || metadata.mode() & 0o777 != DIRECTORY_MODE
    {
        return Err(invalid("pane graphics directory is not private"));
    }
    Ok(())
}

fn validate_path_identity(path: &Path, expected: &fs::Metadata) -> io::Result<()> {
    #[cfg(unix)]
    {
        let actual = fs::symlink_metadata(path)?;
        if actual.dev() != expected.dev() || actual.ino() != expected.ino() {
            return Err(invalid("frame path changed while leased"));
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (path, expected);
        Err(io::Error::new(io::ErrorKind::Unsupported, "Unix only"))
    }
}

#[cfg(unix)]
fn effective_uid() -> u32 {
    // SAFETY: geteuid takes no arguments and has no preconditions.
    unsafe { libc::geteuid() }
}

#[cfg(unix)]
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(unix)]
fn invalid_path() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "invalid pane graphics source path",
    )
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::sync::atomic::AtomicU64;

    static NEXT: AtomicU64 = AtomicU64::new(1);

    fn store() -> (FileStore, PathBuf) {
        let base = PathBuf::from(format!(
            "/var/tmp/herdr-graphics-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        (FileStore::new(base.clone()), base)
    }

    fn frame(store: &FileStore, name: &str, data: &[u8]) -> PathBuf {
        let path = store.source_directory().unwrap().join(name);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(FILE_MODE)
            .open(&path)
            .unwrap();
        file.write_all(data).unwrap();
        path
    }

    #[test]
    fn native_budget_is_per_store_and_released_only_at_final_owner_drop() {
        let store = FileStore::native_sources();
        let other = FileStore::native_sources();
        let budget = store.native_budget.as_ref().unwrap();
        let mut holds = Vec::new();
        for _ in 0..64 {
            holds.push(Reservation::acquire(budget, 4).unwrap());
        }
        assert!(Reservation::acquire(budget, 4).is_err());
        assert!(Reservation::acquire(other.native_budget.as_ref().unwrap(), 4).is_ok());
        holds.clear();
        for _ in 0..4 {
            holds.push(Reservation::acquire(budget, MAX_EXPORT_BYTES).unwrap());
        }
        assert!(Reservation::acquire(budget, 4).is_err());
        holds.clear();
        // Exercise the exact OwnedExport lifetime independently of FS reflink support.
        let export = store.export(&[1, 2, 3, 4]).unwrap();
        let first = Arc::new(export);
        let last = Arc::clone(&first);
        let path = first.path().to_owned();
        drop(first);
        assert_eq!(budget.lock().unwrap().objects, 1);
        assert!(path.exists());
        drop(last);
        assert_eq!(budget.lock().unwrap().objects, 0);
        assert!(!path.exists());
    }

    #[test]
    fn native_validation_is_separate_and_private() {
        let store = FileStore::native_sources();
        let export = store.export(&[1, 2, 3, 4]).unwrap();
        validate_native_source(export.path(), 4).unwrap();
        assert!(validate_direct_source(export.path(), 4).is_err());
        assert!(validate_native_source(export.path(), 8).is_err());
        let decoded = FileStore::default().export(&[1, 2, 3, 4]).unwrap();
        assert!(validate_native_source(decoded.path(), 4).is_err());
        let link = export.path().with_extension("link");
        symlink(export.path(), &link).unwrap();
        assert!(validate_native_source(&link, 4).is_err());
        fs::remove_file(&link).unwrap();
        fs::hard_link(export.path(), &link).unwrap();
        assert!(validate_native_source(export.path(), 4).is_err());
        fs::remove_file(link).unwrap();
        fs::set_permissions(export.path(), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(validate_native_source(export.path(), 4).is_err());
    }

    #[test]
    fn snapshot_validation_releases_budget_and_cleans_staging() {
        let store = FileStore::native_sources();
        for len in [0, 3, MAX_EXPORT_BYTES + 4] {
            assert!(store.snapshot(-1, len).is_err());
        }
        assert!(!store.is_initialized());
        assert!(store.snapshot(-1, 4).is_err());
        assert_eq!(
            store
                .native_budget
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .objects,
            0
        );
        assert_eq!(
            fs::read_dir(store.source_directory().unwrap())
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn snapshot_is_independent_and_preserves_borrowed_fd_offset_when_supported() {
        use std::io::{Seek, SeekFrom};
        use std::os::fd::AsRawFd;
        let store = FileStore::native_sources();
        let path = frame(&store, "producer", &vec![7; 4096]);
        let mut producer = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        producer.seek(SeekFrom::Start(13)).unwrap();
        assert!(store.snapshot(producer.as_raw_fd() as i64, 4).is_err());
        let result = store.snapshot(producer.as_raw_fd() as i64, 4096);
        assert_eq!(producer.stream_position().unwrap(), 13);
        let snapshot = match result {
            Ok(snapshot) => snapshot,
            Err(err) => {
                // Unsupported FS/range/cross-device is a normal parent-reader fallback.
                assert!(
                    err.kind() == io::ErrorKind::Unsupported
                        || matches!(
                            err.raw_os_error(),
                            Some(
                                libc::EOPNOTSUPP
                                    | libc::ENOTTY
                                    | libc::EXDEV
                                    | libc::EINVAL
                                    | libc::ENOSYS
                            )
                        ),
                    "{err}"
                );
                assert_eq!(
                    fs::read_dir(store.source_directory().unwrap())
                        .unwrap()
                        .count(),
                    1
                );
                assert_eq!(
                    store
                        .native_budget
                        .as_ref()
                        .unwrap()
                        .lock()
                        .unwrap()
                        .objects,
                    0
                );
                return;
            }
        };
        producer.write_at(&vec![9; 4096], 0).unwrap();
        assert_eq!(snapshot.copy_rgba().unwrap(), vec![7; 4096]);
        producer.set_len(0).unwrap();
        fs::remove_file(path).unwrap();
        let mut bytes = vec![0; 4096];
        snapshot.read_into(&mut bytes).unwrap();
        assert_eq!(bytes, vec![7; 4096]);
        assert!(snapshot.read_into(&mut [0; 4]).is_err());
        assert_eq!(producer.stream_position().unwrap(), 13);
    }

    #[test]
    fn tmpfs_source_clone_failure_leaves_no_artifact() {
        use std::os::fd::AsRawFd;
        let path = PathBuf::from(format!(
            "/dev/shm/herdr-snapshot-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let Ok(mut producer) = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(FILE_MODE)
            .open(&path)
        else {
            return;
        };
        producer.write_all(&[1, 2, 3, 4]).unwrap();
        fs::remove_file(path).unwrap();
        let store = FileStore::native_sources();
        assert!(store.snapshot(producer.as_raw_fd() as i64, 4).is_err());
        assert_eq!(
            fs::read_dir(store.source_directory().unwrap())
                .unwrap()
                .count(),
            0
        );
        assert_eq!(
            store
                .native_budget
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .objects,
            0
        );
    }

    #[test]
    fn decoded_exports_snapshot_bytes_and_have_distinct_identities() {
        let (store, base) = store();
        let mut data = vec![1, 2, 3, 4];
        let first = store.export(&data).unwrap();
        data.fill(9);
        let second = store.export(&data).unwrap();
        assert_eq!(first.len(), 4);
        assert_eq!(fs::read(first.path()).unwrap(), [1, 2, 3, 4]);
        assert_eq!(first.lease.copy_rgba().unwrap(), [1, 2, 3, 4]);
        assert_eq!(second.lease.copy_rgba().unwrap(), data);
        assert_ne!(first.path(), second.path());
        assert_ne!(first.fingerprint(), second.fingerprint());
        assert_eq!(
            first.path().parent().unwrap(),
            store.source_directory().unwrap()
        );
        validate_metadata(&fs::metadata(first.path()).unwrap(), 4).unwrap();
        let path = first.path().to_owned();
        drop(first);
        assert!(!path.exists());
        assert!(second.path().exists());
        drop(second);
        drop(store);
        let _ = fs::remove_dir(base);
    }

    #[test]
    fn decoded_export_guard_retains_generation_until_consumed() {
        let (store, base) = store();
        let export = store.export(&[1, 2, 3, 4]).unwrap();
        let path = export.path().to_owned();
        let source = store.source_directory().unwrap();
        drop(store);
        assert_eq!(fs::read(&path).unwrap(), [1, 2, 3, 4]);
        assert!(source.exists());
        drop(export);
        assert!(!path.exists());
        assert!(!source.exists());
        let _ = fs::remove_dir(base);
    }

    #[test]
    fn decoded_export_rejects_oversize_and_never_overwrites_collisions() {
        let (store, base) = store();
        assert!(store.export(&vec![0; MAX_EXPORT_BYTES + 1]).is_err());
        assert!(!store.is_initialized());
        let collision = frame(&store, "decoded-1.rgba", &[7, 8]);
        assert!(store.export(&[1, 2, 3, 4]).is_err());
        assert_eq!(fs::read(&collision).unwrap(), [7, 8]);
        assert_eq!(
            fs::read_dir(store.source_directory().unwrap())
                .unwrap()
                .count(),
            1
        );
        let export = store.export(&[1, 2, 3, 4]).unwrap();
        assert_ne!(export.path(), collision);
        drop(export);
        drop(store);
        let _ = fs::remove_dir(base);
    }

    #[test]
    fn unpublished_export_guard_removes_partial_file_on_error() {
        let (store, base) = store();
        let generation = store.generation().unwrap();
        let path = generation.source.join("partial.rgba");
        let result = (|| -> io::Result<()> {
            let mut writer = create_export(&path)?;
            let _cleanup = ExportFile {
                path: path.clone(),
                _generation: generation,
            };
            writer.write_all(&[1, 2])?;
            drop(writer);
            store.lease(&path, 4)?;
            Ok(())
        })();
        assert!(result.is_err());
        assert!(!path.exists());
        assert!(store.source_directory().unwrap().exists());
        drop(store);
        let _ = fs::remove_dir(base);
    }

    #[test]
    fn decoded_export_passes_direct_source_validation() {
        let store = FileStore::default();
        let export = store.export(&[1, 2, 3, 4]).unwrap();
        validate_direct_source(export.path(), export.len()).unwrap();
    }

    #[test]
    fn source_directory_is_lazy_private_and_removed_with_store() {
        let (store, base) = store();
        assert!(!store.is_initialized());
        let source = store.source_directory().unwrap();
        assert!(store.is_initialized());
        validate_directory(&source).unwrap();
        drop(store);
        assert!(!source.exists());
        let _ = fs::remove_dir(base);
    }

    #[test]
    fn lease_holds_exact_file_and_copies_from_validated_fd() {
        let (store, base) = store();
        let path = frame(&store, "frame", &[1, 2, 3, 4]);
        let lease = store.lease(&path, 4).unwrap();
        assert_eq!(lease.path(), path);
        assert_eq!(lease.len(), 4);
        assert!(lease.fingerprint() > 0);
        assert_eq!(lease.copy_rgba().unwrap(), [1, 2, 3, 4]);
        drop(lease);
        drop(store);
        let _ = fs::remove_dir(base);
    }

    #[test]
    fn validation_rejects_links_modes_lengths_and_outside_paths() {
        let cases = ["mode", "length", "hard-link", "symlink", "outside"];
        for case in cases {
            let (store, base) = store();
            let path = frame(&store, "frame", &[1, 2, 3, 4]);
            let result = match case {
                "mode" => {
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
                    store.lease(&path, 4)
                }
                "length" => store.lease(&path, 8),
                "hard-link" => {
                    fs::hard_link(&path, store.source_directory().unwrap().join("other")).unwrap();
                    store.lease(&path, 4)
                }
                "symlink" => {
                    let link = store.source_directory().unwrap().join("link");
                    symlink(&path, &link).unwrap();
                    store.lease(&link, 4)
                }
                "outside" => store.lease(&base.join("outside"), 4),
                _ => unreachable!(),
            };
            assert!(result.is_err(), "{case}");
            drop(store);
            let _ = fs::remove_dir_all(base);
        }
    }

    #[test]
    fn direct_source_validation_accepts_only_the_runtime_generation() {
        let runtime_store = FileStore::default();
        let runtime_path = frame(&runtime_store, "direct", &[1, 2, 3, 4]);
        validate_direct_source(&runtime_path, 4).unwrap();
        assert!(validate_direct_source(&runtime_path, 8).is_err());
        assert!(validate_direct_source(Path::new("relative"), 4).is_err());

        let generation = runtime_path.parent().unwrap().parent().unwrap();
        let wrong_source = generation.join("frames");
        fs::create_dir(&wrong_source).unwrap();
        fs::set_permissions(&wrong_source, fs::Permissions::from_mode(DIRECTORY_MODE)).unwrap();
        let wrong_source_path = wrong_source.join("frame");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(FILE_MODE)
            .open(&wrong_source_path)
            .unwrap();
        file.write_all(&[1, 2, 3, 4]).unwrap();
        assert!(validate_direct_source(&wrong_source_path, 4).is_err());

        let (other_store, other_base) = store();
        let other_path = frame(&other_store, "other", &[1, 2, 3, 4]);
        assert!(validate_direct_source(&other_path, 4).is_err());
        drop(other_store);
        let _ = fs::remove_dir_all(other_base);

        let invalid_generation = runtime_base().join(format!(
            "invalid-generation-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let invalid_source = invalid_generation.join("source");
        fs::create_dir_all(&invalid_source).unwrap();
        fs::set_permissions(
            &invalid_generation,
            fs::Permissions::from_mode(DIRECTORY_MODE),
        )
        .unwrap();
        fs::set_permissions(&invalid_source, fs::Permissions::from_mode(DIRECTORY_MODE)).unwrap();
        let invalid_path = invalid_source.join("frame");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(FILE_MODE)
            .open(&invalid_path)
            .unwrap();
        file.write_all(&[1, 2, 3, 4]).unwrap();
        assert!(validate_direct_source(&invalid_path, 4).is_err());
        let _ = fs::remove_dir_all(invalid_generation);
    }

    #[test]
    fn generation_creation_removes_dead_server_directories() {
        let (store, base) = store();
        fs::create_dir_all(&base).unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(DIRECTORY_MODE)).unwrap();
        let stale = base.join("server-2147483647-stale");
        fs::create_dir(&stale).unwrap();
        fs::set_permissions(&stale, fs::Permissions::from_mode(DIRECTORY_MODE)).unwrap();

        store.source_directory().unwrap();

        assert!(!stale.exists());
        drop(store);
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn replacement_after_lease_is_detected_before_fallback_copy() {
        let (store, base) = store();
        let path = frame(&store, "frame", &[1, 2, 3, 4]);
        let lease = store.lease(&path, 4).unwrap();
        fs::remove_file(&path).unwrap();
        let _replacement = frame(&store, "frame", &[5, 6, 7, 8]);
        assert!(lease.copy_rgba().is_err());
        drop(lease);
        drop(store);
        let _ = fs::remove_dir_all(base);
    }
}
