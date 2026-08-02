#[cfg(unix)]
use std::fs::OpenOptions;
use std::fs::{self, File};
use std::io::{self, Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

#[cfg(unix)]
const DIRECTORY_MODE: u32 = 0o700;
#[cfg(unix)]
const FILE_MODE: u32 = 0o600;

#[derive(Debug)]
pub(crate) struct FileStore {
    base: PathBuf,
    generation: OnceLock<Arc<Generation>>,
    next_fingerprint: AtomicU64,
}

#[derive(Debug)]
struct Generation {
    root: PathBuf,
    source: PathBuf,
}

#[derive(Clone, Debug)]
pub(crate) struct Lease {
    inner: Arc<LeaseInner>,
}

#[derive(Debug)]
struct LeaseInner {
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
        }
    }

    pub(crate) fn source_directory(&self) -> io::Result<PathBuf> {
        Ok(self.generation()?.source.clone())
    }

    pub(crate) fn lease(&self, path: &Path, expected_len: usize) -> io::Result<Lease> {
        let generation = self.generation()?;
        validate_child(path, &generation.source)?;
        let file = open_no_follow(path)?;
        let metadata = file.metadata()?;
        validate_metadata(&metadata, expected_len)?;
        validate_path_identity(path, &metadata)?;
        let fingerprint = self.next_fingerprint.fetch_add(1, Ordering::Relaxed);
        Ok(Lease {
            inner: Arc::new(LeaseInner {
                path: path.to_owned(),
                file,
                generation,
                metadata,
                len: expected_len,
                fingerprint,
            }),
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

    #[cfg(test)]
    pub(crate) fn is_initialized(&self) -> bool {
        self.generation.get().is_some()
    }
}

impl Lease {
    pub(crate) fn path(&self) -> &Path {
        &self.inner.path
    }

    pub(crate) fn len(&self) -> usize {
        self.inner.len
    }

    pub(crate) fn fingerprint(&self) -> u64 {
        self.inner.fingerprint
    }

    pub(crate) fn copy_rgba(&self) -> io::Result<Vec<u8>> {
        let _keep_generation_alive = &self.inner.generation;
        validate_path_identity(&self.inner.path, &self.inner.metadata)?;
        let mut file = self.inner.file.try_clone()?;
        file.rewind()?;
        let mut data = Vec::with_capacity(self.inner.len);
        (&mut file)
            .take(self.inner.len as u64 + 1)
            .read_to_end(&mut data)?;
        if data.len() != self.inner.len {
            return Err(invalid("frame length changed while leased"));
        }
        validate_metadata(&file.metadata()?, self.inner.len)?;
        validate_path_identity(&self.inner.path, &self.inner.metadata)?;
        Ok(data)
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
    let source = path.parent().ok_or_else(invalid_path)?;
    let generation = source.parent().ok_or_else(invalid_path)?;
    if !path.is_absolute()
        || path.file_name().is_none()
        || source.file_name().and_then(|name| name.to_str()) != Some("source")
        || generation.parent() != Some(runtime_base().as_path())
        || !generation
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("server-"))
    {
        return Err(invalid_path());
    }
    for directory in [runtime_base(), generation.to_owned(), source.to_owned()] {
        validate_directory(&directory)?;
    }
    let file = open_no_follow(path)?;
    let metadata = file.metadata()?;
    validate_metadata(&metadata, expected_len)?;
    validate_path_identity(path, &metadata)
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
        fs::create_dir_all(base)?;
        fs::set_permissions(base, fs::Permissions::from_mode(DIRECTORY_MODE))?;
        validate_directory(base)?;
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
        root.join(format!("herdr-pane-graphics-{}", unsafe {
            libc::geteuid()
        }))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from("pane-graphics-unavailable")
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
            || metadata.uid() != unsafe { libc::geteuid() }
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
        || metadata.uid() != unsafe { libc::geteuid() }
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
