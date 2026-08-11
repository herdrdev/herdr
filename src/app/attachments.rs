use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use base64::Engine as _;
use sha2::{Digest, Sha256};

use crate::api::schema::conversations::{
    AgentAttachmentBeginParams, AgentAttachmentChunkParams, AttachmentHandle,
    AttachmentUploadHandle,
};
use crate::layout::PaneId;

pub(crate) const ATTACHMENT_CHUNK_SIZE: usize = 8 * 1024;
const MAX_ATTACHMENT_BYTES: u64 = 25 * 1024 * 1024;
const MAX_TOTAL_ATTACHMENT_BYTES: u64 = 100 * 1024 * 1024;
const MAX_UPLOADS: usize = 32;
const ATTACHMENT_TTL: Duration = Duration::from_secs(15 * 60);
const MAX_NAME_BYTES: usize = 255;
const MAX_MEDIA_TYPE_BYTES: usize = 128;
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AttachmentError(pub(crate) String);

impl std::fmt::Display for AttachmentError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for AttachmentError {}

#[derive(Debug, Clone)]
pub(crate) struct StagedAttachment {
    handle: String,
    pub(crate) pane_id: PaneId,
    pub(crate) session: String,
    pub(crate) media_type: String,
    pub(crate) name: String,
    pub(crate) byte_size: u64,
    pub(crate) path: PathBuf,
    created_at: Instant,
    turn_id: Option<String>,
}

#[derive(Debug)]
struct PendingUpload {
    pane_id: PaneId,
    session: String,
    media_type: String,
    name: String,
    byte_size: u64,
    digest: String,
    next_index: u64,
    received: u64,
    path: PathBuf,
    file: File,
    created_at: Instant,
}

#[derive(Debug)]
pub(crate) struct AttachmentStore {
    root: PathBuf,
    uploads: HashMap<String, PendingUpload>,
    attachments: HashMap<String, StagedAttachment>,
    in_flight: HashMap<String, StagedAttachment>,
}

impl AttachmentStore {
    pub(crate) fn new() -> Self {
        let temp_root = std::env::temp_dir();
        remove_stale_attachment_roots(&temp_root);
        let root = temp_root.join(format!(
            "herdr-attachments-{}-{}",
            std::process::id(),
            NEXT_HANDLE.fetch_add(1, Ordering::Relaxed)
        ));
        Self::new_for_test(root)
    }

    pub(crate) fn new_for_test(root: PathBuf) -> Self {
        let _ = fs::create_dir_all(&root);
        set_private_dir(&root);
        Self {
            root,
            uploads: HashMap::new(),
            attachments: HashMap::new(),
            in_flight: HashMap::new(),
        }
    }

    pub(crate) fn begin(
        &mut self,
        pane_id: PaneId,
        session: &str,
        params: &AgentAttachmentBeginParams,
    ) -> Result<(AttachmentUploadHandle, usize), AttachmentError> {
        self.cleanup_expired();
        if self.uploads.len() + self.attachments.len() + self.in_flight.len() >= MAX_UPLOADS {
            return Err(AttachmentError("attachment quota exceeded".into()));
        }
        validate_media_type(&params.media_type)?;
        validate_name(&params.name)?;
        validate_digest(&params.sha256_digest)?;
        if params.byte_size == 0 || params.byte_size > MAX_ATTACHMENT_BYTES {
            return Err(AttachmentError("attachment size is out of bounds".into()));
        }
        if self.total_bytes().saturating_add(params.byte_size) > MAX_TOTAL_ATTACHMENT_BYTES {
            return Err(AttachmentError(
                "aggregate attachment quota exceeded".into(),
            ));
        }
        if params.target.is_empty() || session.is_empty() {
            return Err(AttachmentError("attachment target is invalid".into()));
        }

        let handle = self.new_handle()?;
        let path = self.root.join(format!("{handle}.part"));
        let file = private_file(&path).map_err(|err| AttachmentError(err.to_string()))?;
        self.uploads.insert(
            handle.clone(),
            PendingUpload {
                pane_id,
                session: session.to_owned(),
                media_type: params.media_type.clone(),
                name: params.name.clone(),
                byte_size: params.byte_size,
                digest: params.sha256_digest.to_ascii_lowercase(),
                next_index: 0,
                received: 0,
                path,
                file,
                created_at: Instant::now(),
            },
        );
        Ok((AttachmentUploadHandle { handle }, ATTACHMENT_CHUNK_SIZE))
    }

    pub(crate) fn chunk(
        &mut self,
        params: AgentAttachmentChunkParams,
    ) -> Result<(), AttachmentError> {
        self.cleanup_expired();
        let upload = self
            .uploads
            .get_mut(&params.upload.handle)
            .ok_or_else(|| AttachmentError("attachment upload is unknown or expired".into()))?;
        if params.index != upload.next_index {
            return Err(AttachmentError("attachment chunks must be ordered".into()));
        }
        let data = base64::engine::general_purpose::STANDARD
            .decode(params.data_base64.as_bytes())
            .map_err(|_| AttachmentError("attachment chunk is not valid base64".into()))?;
        if data.is_empty() || data.len() > ATTACHMENT_CHUNK_SIZE {
            return Err(AttachmentError("attachment chunk is out of bounds".into()));
        }
        if upload.received.saturating_add(data.len() as u64) > upload.byte_size {
            return Err(AttachmentError("attachment exceeds declared size".into()));
        }
        upload
            .file
            .write_all(&data)
            .map_err(|err| AttachmentError(err.to_string()))?;
        upload.received += data.len() as u64;
        upload.next_index += 1;
        Ok(())
    }

    pub(crate) fn finish(
        &mut self,
        upload: AttachmentUploadHandle,
    ) -> Result<AttachmentHandle, AttachmentError> {
        self.cleanup_expired();
        let mut pending = self
            .uploads
            .remove(&upload.handle)
            .ok_or_else(|| AttachmentError("attachment upload is unknown or expired".into()))?;
        if pending.received != pending.byte_size {
            let _ = fs::remove_file(&pending.path);
            return Err(AttachmentError(
                "attachment size does not match declaration".into(),
            ));
        }
        pending.file.flush().map_err(|err| {
            let _ = fs::remove_file(&pending.path);
            AttachmentError(err.to_string())
        })?;
        drop(pending.file);
        let actual = match sha256_file(&pending.path) {
            Ok(actual) => actual,
            Err(err) => {
                let _ = fs::remove_file(&pending.path);
                return Err(AttachmentError(err.to_string()));
            }
        };
        if actual != pending.digest {
            let _ = fs::remove_file(&pending.path);
            return Err(AttachmentError(
                "attachment digest does not match declaration".into(),
            ));
        }
        let handle = self.new_handle()?;
        let final_path = self.root.join(&handle);
        if let Err(err) = fs::rename(&pending.path, &final_path) {
            let _ = fs::remove_file(&pending.path);
            return Err(AttachmentError(err.to_string()));
        }
        set_private_file(&final_path);
        self.attachments.insert(
            handle.clone(),
            StagedAttachment {
                handle: handle.clone(),
                pane_id: pending.pane_id,
                session: pending.session,
                media_type: pending.media_type,
                name: pending.name,
                byte_size: pending.byte_size,
                path: final_path,
                created_at: pending.created_at,
                turn_id: None,
            },
        );
        Ok(AttachmentHandle { handle })
    }

    pub(crate) fn abort_upload(
        &mut self,
        upload: AttachmentUploadHandle,
    ) -> Result<(), AttachmentError> {
        let pending = self
            .uploads
            .remove(&upload.handle)
            .ok_or_else(|| AttachmentError("attachment upload is unknown or expired".into()))?;
        fs::remove_file(pending.path).map_err(|err| AttachmentError(err.to_string()))
    }

    pub(crate) fn resolve(
        &mut self,
        pane_id: PaneId,
        session: &str,
        attachment: &AttachmentHandle,
    ) -> Result<StagedAttachment, AttachmentError> {
        self.cleanup_expired();
        let staged = self
            .attachments
            .get(&attachment.handle)
            .ok_or_else(|| AttachmentError("attachment handle is unknown or expired".into()))?;
        if staged.pane_id != pane_id || staged.session != session {
            return Err(AttachmentError(
                "attachment handle does not belong to this pane session".into(),
            ));
        }
        Ok(staged.clone())
    }

    pub(crate) fn take_for_prompt(
        &mut self,
        attachment: &AttachmentHandle,
    ) -> Result<StagedAttachment, AttachmentError> {
        let mut staged = self
            .attachments
            .remove(&attachment.handle)
            .ok_or_else(|| AttachmentError("attachment handle is unknown or expired".into()))?;
        staged.created_at = Instant::now();
        let returned = staged.clone();
        self.in_flight.insert(attachment.handle.clone(), staged);
        Ok(returned)
    }

    pub(crate) fn discard_prompt_attachments(&mut self, attachments: &[StagedAttachment]) {
        for attachment in attachments {
            if let Some(staged) = self.in_flight.remove(&attachment.handle) {
                let _ = fs::remove_file(staged.path);
            }
        }
    }

    pub(crate) fn bind_prompt_turn(&mut self, pane_id: PaneId, turn_id: &str) {
        for attachment in self.in_flight.values_mut() {
            if attachment.pane_id == pane_id && attachment.turn_id.is_none() {
                attachment.turn_id = Some(turn_id.to_string());
            }
        }
    }

    pub(crate) fn complete_prompt_turn(&mut self, pane_id: PaneId, turn_id: &str) {
        let completed = self
            .in_flight
            .iter()
            .filter(|(_, attachment)| {
                attachment.pane_id == pane_id && attachment.turn_id.as_deref() == Some(turn_id)
            })
            .map(|(handle, _)| handle.clone())
            .collect::<Vec<_>>();
        for handle in completed {
            if let Some(attachment) = self.in_flight.remove(&handle) {
                let _ = fs::remove_file(attachment.path);
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn remove_attachment(
        &mut self,
        attachment: AttachmentHandle,
    ) -> Result<(), AttachmentError> {
        let staged = self
            .attachments
            .remove(&attachment.handle)
            .ok_or_else(|| AttachmentError("attachment handle is unknown or expired".into()))?;
        fs::remove_file(staged.path).map_err(|err| AttachmentError(err.to_string()))
    }

    pub(crate) fn cleanup_expired(&mut self) {
        let now = Instant::now();
        let expired_uploads: Vec<String> = self
            .uploads
            .iter()
            .filter(|(_, upload)| now.duration_since(upload.created_at) >= ATTACHMENT_TTL)
            .map(|(handle, _)| handle.clone())
            .collect();
        for handle in expired_uploads {
            if let Some(upload) = self.uploads.remove(&handle) {
                let _ = fs::remove_file(upload.path);
            }
        }
        let expired_attachments: Vec<String> = self
            .attachments
            .iter()
            .filter(|(_, attachment)| now.duration_since(attachment.created_at) >= ATTACHMENT_TTL)
            .map(|(handle, _)| handle.clone())
            .collect();
        for handle in expired_attachments {
            if let Some(attachment) = self.attachments.remove(&handle) {
                let _ = fs::remove_file(attachment.path);
            }
        }
        let expired_in_flight = self
            .in_flight
            .iter()
            .filter(|(_, attachment)| now.duration_since(attachment.created_at) >= ATTACHMENT_TTL)
            .map(|(handle, _)| handle.clone())
            .collect::<Vec<_>>();
        for handle in expired_in_flight {
            if let Some(attachment) = self.in_flight.remove(&handle) {
                let _ = fs::remove_file(attachment.path);
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn remove_for_pane_session(&mut self, pane_id: PaneId, session: &str) {
        self.remove_matching(|owner_pane, owner_session| {
            owner_pane == pane_id && owner_session == session
        });
    }

    pub(crate) fn remove_for_pane(&mut self, pane_id: PaneId) {
        self.remove_matching(|owner_pane, _| owner_pane == pane_id);
    }

    fn remove_matching(&mut self, matches: impl Fn(PaneId, &str) -> bool) {
        let uploads = self
            .uploads
            .iter()
            .filter(|(_, upload)| matches(upload.pane_id, &upload.session))
            .map(|(handle, _)| handle.clone())
            .collect::<Vec<_>>();
        for handle in uploads {
            if let Some(upload) = self.uploads.remove(&handle) {
                let _ = fs::remove_file(upload.path);
            }
        }

        let attachments = self
            .attachments
            .iter()
            .filter(|(_, attachment)| matches(attachment.pane_id, &attachment.session))
            .map(|(handle, _)| handle.clone())
            .collect::<Vec<_>>();
        for handle in attachments {
            if let Some(attachment) = self.attachments.remove(&handle) {
                let _ = fs::remove_file(attachment.path);
            }
        }

        let in_flight = self
            .in_flight
            .iter()
            .filter(|(_, attachment)| matches(attachment.pane_id, &attachment.session))
            .map(|(handle, _)| handle.clone())
            .collect::<Vec<_>>();
        for handle in in_flight {
            if let Some(attachment) = self.in_flight.remove(&handle) {
                let _ = fs::remove_file(attachment.path);
            }
        }
    }

    fn new_handle(&self) -> Result<String, AttachmentError> {
        for _ in 0..8 {
            let Some(handle) = crate::agent_resume::generate_conversation_handle() else {
                continue;
            };
            if !self.uploads.contains_key(&handle)
                && !self.attachments.contains_key(&handle)
                && !self.in_flight.contains_key(&handle)
            {
                return Ok(handle);
            }
        }
        Err(AttachmentError(
            "failed to allocate an attachment handle".into(),
        ))
    }

    fn total_bytes(&self) -> u64 {
        self.uploads
            .values()
            .map(|upload| upload.byte_size)
            .chain(
                self.attachments
                    .values()
                    .map(|attachment| attachment.byte_size),
            )
            .chain(
                self.in_flight
                    .values()
                    .map(|attachment| attachment.byte_size),
            )
            .sum()
    }
}

impl Drop for AttachmentStore {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[cfg(unix)]
fn remove_stale_attachment_roots(temp_root: &Path) {
    use std::os::unix::fs::MetadataExt as _;

    let Ok(entries) = fs::read_dir(temp_root) else {
        return;
    };
    let current_pid = std::process::id();
    let current_uid = unsafe { libc::geteuid() };
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(remainder) = name.strip_prefix("herdr-attachments-") else {
            continue;
        };
        let Some((pid, suffix)) = remainder.split_once('-') else {
            continue;
        };
        let Ok(pid) = pid.parse::<u32>() else {
            continue;
        };
        if pid == 0
            || pid == current_pid
            || suffix.is_empty()
            || !suffix.bytes().all(|byte| byte.is_ascii_digit())
            || process_is_alive(pid)
        {
            continue;
        }
        let path = entry.path();
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.file_type().is_dir()
            && metadata.uid() == current_uid
            && metadata.mode() & 0o777 == 0o700
        {
            let _ = fs::remove_dir_all(path);
        }
    }
}

#[cfg(not(unix))]
fn remove_stale_attachment_roots(_temp_root: &Path) {}

#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

fn validate_media_type(media_type: &str) -> Result<(), AttachmentError> {
    if media_type.len() > MAX_MEDIA_TYPE_BYTES
        || !media_type.starts_with("image/")
        || media_type.contains(['\r', '\n', '\0'])
    {
        return Err(AttachmentError(
            "attachment media type is not allowed".into(),
        ));
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<(), AttachmentError> {
    if name.is_empty()
        || name.len() > MAX_NAME_BYTES
        || name.contains(['/', '\\', '\r', '\n', '\0'])
        || name.starts_with('.')
    {
        return Err(AttachmentError("attachment name is not allowed".into()));
    }
    Ok(())
}

fn validate_digest(digest: &str) -> Result<(), AttachmentError> {
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(AttachmentError(
            "attachment digest is not valid SHA-256".into(),
        ));
    }
    Ok(())
}

fn private_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

#[cfg(unix)]
fn set_private_dir(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
}

#[cfg(not(unix))]
fn set_private_dir(_: &Path) {}

#[cfg(unix)]
fn set_private_file(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn set_private_file(_: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn begin_params(target: &str, bytes: &[u8]) -> AgentAttachmentBeginParams {
        let digest = Sha256::digest(bytes);
        AgentAttachmentBeginParams {
            target: target.into(),
            media_type: "image/png".into(),
            name: "sample.png".into(),
            byte_size: bytes.len() as u64,
            sha256_digest: digest.iter().map(|byte| format!("{byte:02x}")).collect(),
        }
    }

    #[test]
    fn ordered_chunks_finish_to_private_opaque_attachment() {
        let root =
            std::env::temp_dir().join(format!("herdr-attachment-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mut store = AttachmentStore::new_for_test(root.clone());
        let bytes = b"png bytes";
        let upload = store
            .begin(
                PaneId::from_raw(7),
                "session-a",
                &begin_params("pane-a", bytes),
            )
            .unwrap();
        let chunk = AgentAttachmentChunkParams {
            upload: upload.0.clone(),
            index: 0,
            data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
        };
        store.chunk(chunk).unwrap();
        let attachment = store.finish(upload.0.clone()).unwrap();
        assert_eq!(attachment.handle.len(), 32);
        assert!(!attachment.handle.contains('/'));
        let staged = store
            .resolve(PaneId::from_raw(7), "session-a", &attachment)
            .unwrap();
        assert_eq!(std::fs::read(&staged.path).unwrap(), bytes);
        assert_eq!(staged.media_type, "image/png");
        assert_eq!(staged.name, "sample.png");
        assert_eq!(staged.byte_size, bytes.len() as u64);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&staged.path)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        store.abort_upload(upload.0).unwrap_err();
        store.remove_attachment(attachment).unwrap();
        assert!(!staged.path.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn chunks_are_ordered_and_digest_is_verified() {
        let root =
            std::env::temp_dir().join(format!("herdr-attachment-order-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mut store = AttachmentStore::new_for_test(root.clone());
        let upload = store
            .begin(
                PaneId::from_raw(7),
                "session-a",
                &begin_params("pane-a", b"hello"),
            )
            .unwrap();
        let wrong_order = AgentAttachmentChunkParams {
            upload: upload.0.clone(),
            index: 1,
            data_base64: base64::engine::general_purpose::STANDARD.encode(b"hello"),
        };
        assert!(store.chunk(wrong_order).is_err());
        let bad_digest = AgentAttachmentChunkParams {
            upload: upload.0.clone(),
            index: 0,
            data_base64: base64::engine::general_purpose::STANDARD.encode(b"world"),
        };
        store.chunk(bad_digest).unwrap();
        assert!(store.finish(upload.0).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn attachment_handles_are_bound_to_pane_and_session() {
        let root =
            std::env::temp_dir().join(format!("herdr-attachment-owner-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mut store = AttachmentStore::new_for_test(root.clone());
        let upload = store
            .begin(
                PaneId::from_raw(7),
                "session-a",
                &begin_params("pane-a", b"x"),
            )
            .unwrap();
        store
            .chunk(AgentAttachmentChunkParams {
                upload: upload.0.clone(),
                index: 0,
                data_base64: base64::engine::general_purpose::STANDARD.encode(b"x"),
            })
            .unwrap();
        let attachment = store.finish(upload.0).unwrap();
        assert!(store
            .resolve(PaneId::from_raw(8), "session-a", &attachment)
            .is_err());
        assert!(store
            .resolve(PaneId::from_raw(7), "session-b", &attachment)
            .is_err());
        assert!(store
            .resolve(PaneId::from_raw(7), "session-a", &attachment)
            .is_ok());
    }

    #[test]
    fn prompt_attachment_survives_submission_until_its_turn_finishes() {
        let root = std::env::temp_dir().join(format!(
            "herdr-attachment-prompt-lifecycle-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let mut store = AttachmentStore::new_for_test(root);
        let upload = store
            .begin(
                PaneId::from_raw(7),
                "session-a",
                &begin_params("pane-a", b"image"),
            )
            .unwrap()
            .0;
        store
            .chunk(AgentAttachmentChunkParams {
                upload: upload.clone(),
                index: 0,
                data_base64: base64::engine::general_purpose::STANDARD.encode(b"image"),
            })
            .unwrap();
        let handle = store.finish(upload).unwrap();
        let staged = store.take_for_prompt(&handle).unwrap();

        assert!(staged.path.exists());
        assert!(store
            .resolve(PaneId::from_raw(7), "session-a", &handle)
            .is_err());
        store.bind_prompt_turn(PaneId::from_raw(7), "turn-1");
        store.complete_prompt_turn(PaneId::from_raw(7), "other-turn");
        assert!(staged.path.exists());

        store.complete_prompt_turn(PaneId::from_raw(7), "turn-1");
        assert!(!staged.path.exists());
        assert!(!store.in_flight.contains_key(&handle.handle));
    }

    #[test]
    fn attachment_handles_are_random_opaque_and_unique() {
        let root =
            std::env::temp_dir().join(format!("herdr-attachment-opaque-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mut store = AttachmentStore::new_for_test(root);

        let first = store
            .begin(
                PaneId::from_raw(7),
                "session-a",
                &begin_params("pane-a", b"a"),
            )
            .unwrap()
            .0;
        let second = store
            .begin(
                PaneId::from_raw(7),
                "session-a",
                &begin_params("pane-a", b"b"),
            )
            .unwrap()
            .0;

        assert_ne!(first, second);
        assert!(!first.handle.starts_with("upload-"));
        assert_eq!(first.handle.len(), 32);
        assert!(first.handle.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    #[test]
    fn expired_and_replaced_session_attachments_are_removed() {
        let root =
            std::env::temp_dir().join(format!("herdr-attachment-cleanup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mut store = AttachmentStore::new_for_test(root);

        let expired_upload = store
            .begin(
                PaneId::from_raw(7),
                "session-old",
                &begin_params("pane-a", b"pending"),
            )
            .unwrap()
            .0;
        store
            .uploads
            .get_mut(&expired_upload.handle)
            .unwrap()
            .created_at = Instant::now() - ATTACHMENT_TTL;
        let expired_path = store.uploads[&expired_upload.handle].path.clone();

        let completed_upload = store
            .begin(
                PaneId::from_raw(7),
                "session-old",
                &begin_params("pane-a", b"complete"),
            )
            .unwrap()
            .0;
        store
            .chunk(AgentAttachmentChunkParams {
                upload: completed_upload.clone(),
                index: 0,
                data_base64: base64::engine::general_purpose::STANDARD.encode(b"complete"),
            })
            .unwrap();
        let completed = store.finish(completed_upload).unwrap();
        let completed_path = store.attachments[&completed.handle].path.clone();
        store.take_for_prompt(&completed).unwrap();

        store.cleanup_expired();
        assert!(!expired_path.exists());
        assert!(!store.uploads.contains_key(&expired_upload.handle));

        store.remove_for_pane_session(PaneId::from_raw(7), "session-old");
        assert!(!completed_path.exists());
        assert!(!store.attachments.contains_key(&completed.handle));
        assert!(!store.in_flight.contains_key(&completed.handle));
    }

    #[test]
    fn maximum_attachment_round_trips_in_bounded_chunks() {
        let root =
            std::env::temp_dir().join(format!("herdr-attachment-max-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mut store = AttachmentStore::new_for_test(root);
        let bytes = vec![0x5a; MAX_ATTACHMENT_BYTES as usize];
        let (upload, chunk_size) = store
            .begin(
                PaneId::from_raw(7),
                "session-a",
                &begin_params("pane-a", &bytes),
            )
            .unwrap();

        assert_eq!(chunk_size, ATTACHMENT_CHUNK_SIZE);
        for (index, chunk) in bytes.chunks(chunk_size).enumerate() {
            let encoded = base64::engine::general_purpose::STANDARD.encode(chunk);
            assert!(encoded.len() < 1024 * 1024);
            store
                .chunk(AgentAttachmentChunkParams {
                    upload: upload.clone(),
                    index: index as u64,
                    data_base64: encoded,
                })
                .unwrap();
        }
        let attachment = store.finish(upload).unwrap();
        let staged = store
            .resolve(PaneId::from_raw(7), "session-a", &attachment)
            .unwrap();
        assert_eq!(staged.byte_size, MAX_ATTACHMENT_BYTES);
        assert_eq!(
            std::fs::metadata(staged.path).unwrap().len(),
            MAX_ATTACHMENT_BYTES
        );
    }

    #[cfg(unix)]
    #[test]
    fn new_store_removes_secure_attachment_roots_from_dead_processes() {
        let stale_root = std::env::temp_dir().join(format!("herdr-attachments-{}-999", i32::MAX));
        let _ = std::fs::remove_dir_all(&stale_root);
        std::fs::create_dir(&stale_root).unwrap();
        set_private_dir(&stale_root);
        std::fs::write(stale_root.join("leftover"), b"stale").unwrap();

        let store = AttachmentStore::new();

        assert!(!stale_root.exists());
        drop(store);
    }
}
