//! Durable, at-most-once dispatch intent. Receipts deliberately distinguish queue acceptance
//! from agent acceptance. A crash anywhere after claiming an operation leaves an unknown result.
use crate::api::schema::{InteractionOutcome, InteractionReceipt, InteractionSubmitParams};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

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

fn operation_dir(root: &Path, id: &str) -> PathBuf {
    root.join(digest(id.as_bytes()))
}

pub(crate) fn lookup(root: &Path, id: &str) -> io::Result<Option<InteractionReceipt>> {
    let dir = operation_dir(root, id);
    match fs::metadata(&dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
        Ok(_) => {}
    }
    let intent: InteractionReceipt =
        serde_json::from_slice(&fs::read(dir.join("intent.json"))?).map_err(io::Error::other)?;
    if intent.operation_id != id || intent.outcome != InteractionOutcome::UnknownDelivery {
        return Err(io::Error::other("invalid operation intent"));
    }
    match fs::read(dir.join("receipt.json")) {
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

fn write_new(path: &Path, value: &InteractionReceipt) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    let mut file = crate::platform::create_private_state_file(path)?;
    file.write_all(&bytes)?;
    file.sync_all()
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
    if let Some(receipt) = lookup(root, &params.operation_id)? {
        if receipt.payload_digest != params.payload_digest {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "operation ID already binds another payload",
            ));
        }
        return Ok(receipt);
    }
    match crate::platform::create_private_state_directory(root) {
        Ok(()) => crate::platform::sync_parent_directory(
            root.parent()
                .ok_or_else(|| io::Error::other("missing journal parent"))?,
        )?,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    let dir = operation_dir(root, &params.operation_id);
    // create_new directory prevents a second dispatcher from claiming the same operation.
    // A concurrent claim, partial intent or corrupt receipt is an error, never a redispatch.
    crate::platform::create_private_state_directory(&dir)?;
    crate::platform::sync_parent_directory(root)?;
    let intent = InteractionReceipt {
        operation_id: params.operation_id.clone(),
        payload_digest: params.payload_digest.clone(),
        outcome: InteractionOutcome::UnknownDelivery,
        code: Some("unresolved_intent".into()),
    };
    write_new(&dir.join("intent.json"), &intent)?;
    crate::platform::sync_parent_directory(&dir)?;
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
    write_new(&dir.join("receipt.json"), &receipt)?;
    crate::platform::sync_parent_directory(&dir)?;
    Ok(receipt)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::api::schema::*;
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
        fs::create_dir(&root).unwrap();
        fs::create_dir(operation_dir(&root, "op1")).unwrap();
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
}
