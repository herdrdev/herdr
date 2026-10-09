//! Durable, at-most-once dispatch intent. Receipts deliberately distinguish queue acceptance
//! from agent acceptance. A crash anywhere after claiming an operation leaves an unknown result.
use crate::api::schema::{
    InteractionDialog, InteractionOutcome, InteractionReceipt, InteractionSubmitParams,
};
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

/// Validated preflight context is a separate durable record, written only after the exact
/// observation and native dialog have been checked and before any queue submission. It is
/// not delivery proof. Old intents cannot be upgraded into parent custom-entry authority.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ValidatedLineage {
    pub request: InteractionSubmitParams,
    pub dialog: InteractionDialog,
    pub encoded_input_digest: String,
}
struct PreparedInput {
    bytes: Vec<u8>,
    dialog: Option<InteractionDialog>,
}

#[cfg(unix)]
pub(crate) fn validated_lineage(root: &Path, id: &str) -> io::Result<Option<ValidatedLineage>> {
    if !valid_operation_id(id) {
        return Err(io::Error::other("invalid parent operation ID"));
    }
    let root = trusted::root(root, false)?;
    let Some(receipt) = lookup_in(&root, id)? else {
        return Ok(None);
    };
    let dir = trusted::operation(&root, id)?;
    let bytes = match trusted::read(&dir, "validated.json") {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        other => other?,
    };
    let lineage: ValidatedLineage = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    let request: InteractionSubmitParams =
        serde_json::from_slice(&trusted::read(&dir, "request.json")?).map_err(io::Error::other)?;
    if request != lineage.request {
        return Err(io::Error::other(
            "validated request does not match durable request",
        ));
    }
    if lineage.request.operation_id != id
        || lineage.request.payload_digest != receipt.payload_digest
        || payload_digest(&lineage.request)? != receipt.payload_digest
    {
        return Err(io::Error::other("validated context does not match intent"));
    }
    Ok(Some(lineage))
}

#[cfg(not(unix))]
pub(crate) fn validated_lineage(_: &Path, _: &str) -> io::Result<Option<ValidatedLineage>> {
    Err(io::Error::other("unsupported journal platform"))
}

/// The exclusive parent claim contains the complete child request and is synced before any
/// child input. Partial failures permanently consume the parent and cannot authorize retries.
pub(crate) fn claim_custom_child(
    root: &Path,
    parent_id: &str,
    child: &InteractionSubmitParams,
) -> io::Result<()> {
    if !valid_operation_id(parent_id)
        || parent_id == child.operation_id
        || payload_digest(child)? != child.payload_digest
    {
        return Err(io::Error::other("invalid custom child request"));
    }
    #[cfg(unix)]
    {
        let root = trusted::root(root, false)?;
        let dir = trusted::operation(&root, parent_id)?;
        trusted::write(&dir, "custom-child.json", child)
    }
    #[cfg(not(unix))]
    {
        let _ = root;
        Err(io::Error::other("unsupported journal platform"))
    }
}

pub(crate) fn dispatch_guarded(
    root: &Path,
    params: &InteractionSubmitParams,
    prepare: impl FnOnce() -> Result<(Vec<u8>, InteractionDialog), &'static str>,
    send: impl FnOnce(Vec<u8>) -> io::Result<()>,
) -> io::Result<InteractionReceipt> {
    dispatch_inner(
        root,
        params,
        true,
        || {
            prepare().map(|(bytes, dialog)| PreparedInput {
                bytes,
                dialog: Some(dialog),
            })
        },
        send,
    )
}

#[cfg(test)]
pub(crate) fn dispatch(
    root: &Path,
    params: &InteractionSubmitParams,
    prepare: impl FnOnce() -> Result<Vec<u8>, &'static str>,
    send: impl FnOnce(Vec<u8>) -> io::Result<()>,
) -> io::Result<InteractionReceipt> {
    dispatch_inner(
        root,
        params,
        false,
        || {
            prepare().map(|bytes| PreparedInput {
                bytes,
                dialog: None,
            })
        },
        send,
    )
}

/// Caller must serialize runtime validation and queue submission. No raw byte API is exposed
/// publicly: `prepare` must verify a native profile and compile a typed action itself.
fn dispatch_inner(
    root: &Path,
    params: &InteractionSubmitParams,
    retain_request: bool,
    prepare: impl FnOnce() -> Result<PreparedInput, &'static str>,
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
        if retain_request {
            trusted::write(&dir, "request.json", params)?;
        }
        let mut receipt = intent.clone();
        match prepare() {
            Err(code) => {
                receipt.outcome = InteractionOutcome::Rejected;
                receipt.code = Some(code.into());
            }
            Ok(prepared) => {
                if let Some(dialog) = prepared.dialog {
                    // Exclusive durable write: failure leaves unknown intent and sends nothing.
                    trusted::write(
                        &dir,
                        "validated.json",
                        &ValidatedLineage {
                            request: params.clone(),
                            dialog,
                            encoded_input_digest: digest(&prepared.bytes),
                        },
                    )?;
                }
                match send(prepared.bytes) {
                    Ok(()) => {
                        receipt.outcome = InteractionOutcome::Enqueued;
                        receipt.code = None;
                    }
                    Err(_) => receipt.code = Some("queue_submission_failed".into()),
                }
            }
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
                style_digest: None,
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
    fn interaction_guarded_lineage_precedes_queue_and_crash_cannot_retrofit_or_resend() {
        let root = root();
        let p = params();
        let dialog = InteractionDialog {
            profile: "test-native".into(),
            phase: "choose".into(),
            question: "What format?".into(),
            options: vec![],
            selected_option_id: "choice-1".into(),
            supported_actions: vec![],
        };
        let crash = std::panic::catch_unwind(|| {
            dispatch_guarded(
                &root,
                &p,
                || Ok((b"4".to_vec(), dialog.clone())),
                |_| {
                    let saved = validated_lineage(&root, &p.operation_id).unwrap().unwrap();
                    assert_eq!(saved.request, p);
                    assert_eq!(saved.dialog, dialog);
                    assert_eq!(saved.encoded_input_digest, digest(b"4"));
                    assert_eq!(
                        lookup(&root, &p.operation_id).unwrap().unwrap().outcome,
                        InteractionOutcome::UnknownDelivery
                    );
                    panic!("crash after validated prewrite");
                },
            )
        });
        assert!(crash.is_err());
        assert_eq!(
            dispatch_guarded(&root, &p, || panic!("prepare"), |_| panic!("resend"))
                .unwrap()
                .outcome,
            InteractionOutcome::UnknownDelivery
        );
        fs::remove_dir_all(root).unwrap();
        let old_root = self::root();
        let _ = std::panic::catch_unwind(|| {
            dispatch(&old_root, &p, || Ok(vec![13]), |_| panic!("legacy intent"))
        });
        assert!(validated_lineage(&old_root, &p.operation_id)
            .unwrap()
            .is_none());
        let _ =
            dispatch_guarded(&old_root, &p, || panic!("retrofit"), |_| panic!("resend")).unwrap();
        assert!(validated_lineage(&old_root, &p.operation_id)
            .unwrap()
            .is_none());
        fs::remove_dir_all(old_root).unwrap();
    }
    #[test]
    fn interaction_preexisting_validated_record_prevents_queue_and_remains_unknown() {
        let root = root();
        let p = params();
        let result = dispatch_guarded(
            &root,
            &p,
            || {
                let dir =
                    trusted::operation(&trusted::root(&root, false).unwrap(), &p.operation_id)
                        .unwrap();
                trusted::write(
                    &dir,
                    "validated.json",
                    &serde_json::json!({"malformed":true}),
                )
                .unwrap();
                Ok((
                    b"4".to_vec(),
                    InteractionDialog {
                        profile: "test-native".into(),
                        phase: "choose".into(),
                        question: "What format?".into(),
                        options: vec![],
                        selected_option_id: "choice-1".into(),
                        supported_actions: vec![],
                    },
                ))
            },
            |_| panic!("queue after existing record"),
        );
        assert!(result.is_err());
        assert!(validated_lineage(&root, &p.operation_id).is_err());
        assert_eq!(
            lookup(&root, &p.operation_id).unwrap().unwrap().outcome,
            InteractionOutcome::UnknownDelivery
        );
        assert_eq!(
            dispatch_guarded(&root, &p, || panic!("prepare"), |_| panic!("resend"))
                .unwrap()
                .outcome,
            InteractionOutcome::UnknownDelivery
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn interaction_custom_child_complete_request_and_parent_claim_precede_queue_and_survive_crash()
    {
        let root = root();
        let mut parent = params();
        parent.operation_id = "parent".into();
        parent.action = InteractionAction::BeginCustom {
            option_id: "choice-4".into(),
        };
        parent.payload_digest = payload_digest(&parent).unwrap();
        let dialog = InteractionDialog {
            profile: "test".into(),
            phase: "choose".into(),
            question: "Format?".into(),
            options: vec![],
            selected_option_id: "choice-1".into(),
            supported_actions: vec![],
        };
        dispatch_guarded(
            &root,
            &parent,
            || Ok((b"4".to_vec(), dialog.clone())),
            |_| Ok(()),
        )
        .unwrap();
        let mut child = params();
        child.operation_id = "child".into();
        child.action = InteractionAction::SubmitCustom {
            text: "A library circle.".into(),
            parent_operation_id: "parent".into(),
        };
        child.payload_digest = payload_digest(&child).unwrap();
        let crash = std::panic::catch_unwind(|| {
            dispatch_guarded(
                &root,
                &child,
                || {
                    let dir =
                        trusted::operation(&trusted::root(&root, false).unwrap(), "child").unwrap();
                    let stored: InteractionSubmitParams =
                        serde_json::from_slice(&trusted::read(&dir, "request.json").unwrap())
                            .unwrap();
                    assert_eq!(
                        stored, child,
                        "complete child request must exist even before parent claim"
                    );
                    claim_custom_child(&root, "parent", &child).unwrap();
                    Ok((b"answer-paste-and-enter".to_vec(), dialog.clone()))
                },
                |bytes| {
                    let dir = trusted::operation(&trusted::root(&root, false).unwrap(), "parent")
                        .unwrap();
                    let claim: InteractionSubmitParams =
                        serde_json::from_slice(&trusted::read(&dir, "custom-child.json").unwrap())
                            .unwrap();
                    assert_eq!(claim, child);
                    assert_eq!(
                        validated_lineage(&root, "child")
                            .unwrap()
                            .unwrap()
                            .encoded_input_digest,
                        digest(&bytes)
                    );
                    panic!("crash after child queue marker");
                },
            )
        });
        assert!(crash.is_err());
        assert_eq!(
            dispatch_guarded(&root, &child, || panic!("prepare"), |_| panic!("resend"))
                .unwrap()
                .outcome,
            InteractionOutcome::UnknownDelivery
        );
        child.operation_id = "other-child".into();
        child.payload_digest = payload_digest(&child).unwrap();
        let rejected = dispatch_guarded(
            &root,
            &child,
            || {
                claim_custom_child(&root, "parent", &child)
                    .map_err(|_| "custom_parent_consumed")?;
                panic!("claimed twice")
            },
            |_| panic!("second child queue"),
        )
        .unwrap();
        assert_eq!(rejected.outcome, InteractionOutcome::Rejected);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn interaction_partial_parent_claim_prevents_every_child_queue() {
        let root = root();
        let parent = params();
        dispatch_guarded(
            &root,
            &parent,
            || {
                Ok((
                    b"4".to_vec(),
                    InteractionDialog {
                        profile: "test".into(),
                        phase: "choose".into(),
                        question: "Format?".into(),
                        options: vec![],
                        selected_option_id: "choice-1".into(),
                        supported_actions: vec![],
                    },
                ))
            },
            |_| Ok(()),
        )
        .unwrap();
        let dir = trusted::operation(&trusted::root(&root, false).unwrap(), &parent.operation_id)
            .unwrap();
        trusted::write(
            &dir,
            "custom-child.json",
            &serde_json::json!({"partial":true}),
        )
        .unwrap();
        let mut child = params();
        child.operation_id = "child".into();
        child.payload_digest = payload_digest(&child).unwrap();
        let result = dispatch_guarded(
            &root,
            &child,
            || {
                assert!(operation_dir(&root, "child").join("request.json").exists());
                claim_custom_child(&root, &parent.operation_id, &child)
                    .map_err(|_| "custom_parent_consumed")?;
                panic!("claimed partial parent")
            },
            |_| panic!("queued partial parent"),
        )
        .unwrap();
        assert_eq!(result.outcome, InteractionOutcome::Rejected);
        assert!(validated_lineage(&root, "child").unwrap().is_none());
        fs::remove_dir_all(root).unwrap();
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
