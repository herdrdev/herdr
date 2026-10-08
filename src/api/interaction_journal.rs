//! Durable, at-most-once dispatch intent. Receipts deliberately distinguish queue acceptance
//! from agent acceptance. A crash anywhere after claiming an operation leaves an unknown result.
use crate::api::schema::{InteractionOutcome, InteractionReceipt, InteractionSubmitParams};
use sha2::{Digest, Sha256};
use std::{fs, io, path::Path};

/// Invalidates observations across process restart/handoff, even if IDs and sequence reset.
pub(crate) fn server_instance_id() -> &'static str {
    static INSTANCE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    INSTANCE.get_or_init(|| {
        format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        )
    })
}

pub(crate) fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(crate) fn payload_digest(params: &InteractionSubmitParams) -> io::Result<String> {
    serde_json::to_vec(&(&params.operation_id, &params.expected, &params.action))
        .map(|bytes| digest(&bytes))
        .map_err(io::Error::other)
}

pub(crate) fn valid_operation_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

#[cfg(test)]
fn operation_dir(root: &Path, id: &str) -> std::path::PathBuf {
    root.join(digest(id.as_bytes()))
}

#[cfg(unix)]
use crate::platform::interaction_journal_fs as trusted;

#[cfg(unix)]
fn lookup_in(root: &fs::File, id: &str) -> io::Result<Option<InteractionReceipt>> {
    let dir = match trusted::operation(root, id) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        other => other?,
    };
    let intent: InteractionReceipt =
        serde_json::from_slice(&trusted::read(&dir, "intent.json")?).map_err(io::Error::other)?;
    if intent.operation_id != id || intent.outcome != InteractionOutcome::UnknownDelivery {
        return Err(io::Error::other("invalid operation intent"));
    }
    match trusted::read(&dir, "receipt.json") {
        Ok(bytes) => {
            let receipt: InteractionReceipt =
                serde_json::from_slice(&bytes).map_err(io::Error::other)?;
            if receipt.operation_id != id || receipt.payload_digest != intent.payload_digest {
                return Err(io::Error::other("receipt does not match intent"));
            }
            Ok(Some(receipt))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Some(intent)),
        Err(e) => Err(e),
    }
}
pub(crate) fn lookup(root: &Path, id: &str) -> io::Result<Option<InteractionReceipt>> {
    if !valid_operation_id(id) {
        return Err(io::Error::other("invalid operation ID"));
    }
    #[cfg(unix)]
    {
        let root = match trusted::root(root, false) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            other => other?,
        };
        lookup_in(&root, id)
    }
    #[cfg(not(unix))]
    {
        let _ = root;
        Err(io::Error::other("unsupported journal platform"))
    }
}

/// Caller must serialize runtime validation and queue submission. No raw byte API is exposed
/// publicly: `prepare` must verify a native profile and compile a typed action itself.
pub(crate) fn dispatch(
    root: &Path,
    params: &InteractionSubmitParams,
    prepare: impl FnOnce() -> Result<Vec<u8>, &'static str>,
    send: impl FnOnce(Vec<u8>) -> io::Result<()>,
) -> io::Result<InteractionReceipt> {
    if !crate::platform::DURABLE_INTERACTION_JOURNAL_SUPPORTED {
        return Err(io::Error::other(
            "durable interaction journal unsupported on this platform",
        ));
    }
    if !valid_operation_id(&params.operation_id) || payload_digest(params)? != params.payload_digest
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid operation ID or payload digest",
        ));
    }
    #[cfg(not(unix))]
    return Err(io::Error::other("unsupported journal platform"));
    #[cfg(unix)]
    {
        let root = trusted::root(root, true)?;
        if let Some(receipt) = lookup_in(&root, &params.operation_id)? {
            if receipt.payload_digest != params.payload_digest {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "operation ID already binds another payload",
                ));
            }
            return Ok(receipt);
        }
        trusted::mkdir(
            &root,
            std::ffi::OsStr::new(&digest(params.operation_id.as_bytes())),
        )?;
        let dir = trusted::operation(&root, &params.operation_id)?;
        let intent = InteractionReceipt {
            operation_id: params.operation_id.clone(),
            payload_digest: params.payload_digest.clone(),
            outcome: InteractionOutcome::UnknownDelivery,
            code: Some("unresolved_intent".into()),
        };
        trusted::write(&dir, "intent.json", &intent)?;
        let mut receipt = intent.clone();
        match prepare() {
            Err(code) => {
                receipt.outcome = InteractionOutcome::Rejected;
                receipt.code = Some(code.into());
            }
            Ok(bytes) => match send(bytes) {
                Ok(()) => {
                    receipt.outcome = InteractionOutcome::Enqueued;
                    receipt.code = None;
                }
                Err(_) => receipt.code = Some("queue_submission_failed".into()),
            },
        }
        // Never overwrite a prior result. Even receipt-write failure leaves the durable intent,
        // so the operation can only be queried/reconciled, never submitted again.
        trusted::write(&dir, "receipt.json", &receipt)?;
        Ok(receipt)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::api::schema::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    fn root() -> PathBuf {
        std::env::temp_dir().join(format!(
            "herdr-interaction-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }
    fn params() -> InteractionSubmitParams {
        let mut p = InteractionSubmitParams {
            operation_id: "op1".into(),
            payload_digest: String::new(),
            expected: InteractionObservation {
                terminal_id: "t1".into(),
                server_instance_id: "instance1".into(),
                runtime_pid: 123,
                agent: "codex".into(),
                state_change_seq: 1,
                content_digest: digest(b"dialog"),
                agent_session: AgentSessionInfo {
                    source: "hook".into(),
                    agent: "codex".into(),
                    kind: crate::agent_resume::AgentSessionRefKind::Id,
                    value: "session1".into(),
                },
            },
            action: InteractionAction::FreeText {
                text: "real answer".into(),
            },
        };
        p.payload_digest = payload_digest(&p).unwrap();
        p
    }
    #[test]
    fn interaction_journal_intent_precedes_send_and_retry_never_sends() {
        let root = root();
        let p = params();
        let receipt = dispatch(
            &root,
            &p,
            || Ok(b"answer\r".to_vec()),
            |_| {
                assert_eq!(
                    lookup(&root, "op1").unwrap().unwrap().outcome,
                    InteractionOutcome::UnknownDelivery
                );
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(receipt.outcome, InteractionOutcome::Enqueued);
        assert_eq!(
            dispatch(
                &root,
                &p,
                || panic!("retry validation"),
                |_| panic!("retry send")
            )
            .unwrap(),
            receipt
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn interaction_journal_crash_after_intent_cannot_redispatch() {
        let root = root();
        let p = params();
        let crash = std::panic::catch_unwind(|| {
            dispatch(
                &root,
                &p,
                || Ok(vec![13]),
                |_| panic!("crash before receipt"),
            )
        });
        assert!(crash.is_err());
        assert_eq!(
            dispatch(&root, &p, || panic!("prepare"), |_| panic!("send"))
                .unwrap()
                .outcome,
            InteractionOutcome::UnknownDelivery
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn interaction_journal_collision_corruption_and_rejection_fail_closed() {
        let root = root();
        let mut p = params();
        let r = dispatch(&root, &p, || Err("stale_content"), |_| panic!("send")).unwrap();
        assert_eq!(r.outcome, InteractionOutcome::Rejected);
        p.action = InteractionAction::FreeText {
            text: "different".into(),
        };
        assert!(dispatch(&root, &p, || panic!("prepare"), |_| panic!("send")).is_err());
        p.payload_digest = payload_digest(&p).unwrap();
        assert!(dispatch(&root, &p, || panic!("prepare"), |_| panic!("send")).is_err());
        fs::write(operation_dir(&root, "op1").join("receipt.json"), b"partial").unwrap();
        assert!(dispatch(&root, &p, || panic!("prepare"), |_| panic!("send")).is_err());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn interaction_journal_partial_claim_and_queue_error_never_retry() {
        let root = root();
        let p = params();
        crate::platform::create_private_state_directory(&root).unwrap();
        crate::platform::create_private_state_directory(&operation_dir(&root, "op1")).unwrap();
        assert!(dispatch(&root, &p, || panic!("prepare"), |_| panic!("send")).is_err());
        fs::remove_dir_all(&root).unwrap();
        let r = dispatch(
            &root,
            &p,
            || Ok(vec![13]),
            |_| Err(io::Error::other("closed")),
        )
        .unwrap();
        assert_eq!(r.outcome, InteractionOutcome::UnknownDelivery);
        assert_eq!(
            dispatch(&root, &p, || panic!("prepare"), |_| panic!("send")).unwrap(),
            r
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn interaction_journal_rejects_unsafe_roots_ancestors_and_linked_records() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let root = root();
        let p = params();
        fs::create_dir(&root).unwrap();
        assert!(dispatch(&root, &p, || panic!("prepare"), |_| panic!("send")).is_err());
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        dispatch(&root, &p, || Err("test"), |_| panic!("send")).unwrap();
        let receipt = operation_dir(&root, "op1").join("receipt.json");
        fs::set_permissions(&receipt, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(lookup(&root, "op1").is_err());
        fs::set_permissions(&receipt, fs::Permissions::from_mode(0o600)).unwrap();
        let link = root.join("hardlink");
        fs::hard_link(&receipt, &link).unwrap();
        assert!(lookup(&root, "op1").is_err());
        fs::remove_file(link).unwrap();
        fs::remove_file(&receipt).unwrap();
        symlink("intent.json", &receipt).unwrap();
        assert!(lookup(&root, "op1").is_err());
        fs::remove_file(&receipt).unwrap();
        fs::set_permissions(
            operation_dir(&root, "op1"),
            fs::Permissions::from_mode(0o777),
        )
        .unwrap();
        assert!(lookup(&root, "op1").is_err());
        fs::remove_dir_all(&root).unwrap();
        let destination = root.with_extension("destination");
        crate::platform::create_private_state_directory(&destination).unwrap();
        symlink(&destination, &root).unwrap();
        assert!(dispatch(&root, &p, || panic!("prepare"), |_| panic!("send")).is_err());
        fs::remove_file(&root).unwrap();
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(dispatch(
            &root.join("journal"),
            &p,
            || panic!("prepare"),
            |_| panic!("send")
        )
        .is_err());
        fs::remove_dir_all(&root).unwrap();
        symlink(&destination, &root).unwrap();
        assert!(dispatch(
            &root.join("journal"),
            &p,
            || panic!("prepare"),
            |_| panic!("send")
        )
        .is_err());
        fs::remove_file(&root).unwrap();
        fs::remove_dir_all(destination).unwrap();
    }
}
