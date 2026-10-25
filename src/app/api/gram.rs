//! Gram message handlers: the owner<->agent channel surfaced in the app.
//!
//! `gram.send` (agent->owner, push-notified), `gram.post` (owner->agent, shared
//! queue or direct), `gram.list` (audience inferred from the caller pane),
//! `gram.grab` (first-wins claim of a shared item), and `gram.mark_read`.
//!
//! Identity and its guarantees. There is no per-connection identity, so the
//! caller passes its `HERDR_PANE_ID` as `caller_pane_id` and the server resolves
//! it (see [`App::caller_identity`]) to a single label: the agent's **name** when
//! one is set, else the pane's public id. The agent name is the durable choice —
//! it is persisted in the session snapshot and restored across a restart or a
//! live-handoff (the deploy path), which the terminal id is not. It is, however,
//! a NAME: renaming or clearing an agent, moving an unnamed pane between
//! workspaces, or reusing a freed name changes or transfers the identity, and a
//! message or claim is attributed to the identity at the moment it was written.
//! That is deliberate name-semantics, not a safety property: **the grab is
//! first-wins atomic at the storage layer regardless of identity, so no two
//! agents can ever claim the same item.** Identity affects only which items a
//! caller sees as "mine" in the agent view. A durable, immutable, non-reusable
//! identity is tracked as a follow-up.
//!
//! The owner's app sends no `caller_pane_id` (owner view = everything); a
//! `caller_pane_id` that names no live pane is an error, not a silent
//! fall-through to the owner view. Sender/owner attribution is advisory, not
//! authenticated — the trust domain is already flat.
//!
//! The `gram.delete` and `gram.get_file` audience checks carry the same caveat:
//! an agent that supplies its caller pane may only delete or download a message
//! it can see, but "owner" is simply the ABSENCE of a caller pane, so a local
//! caller that omits it acts with owner authority. This is COOPERATIVE FILTERING
//! within a flat trust domain — every local process can already read `gram.json`
//! and the blob files directly — not authenticated isolation, and it must not be
//! relied on to hide a secret from a determined co-resident agent. A
//! capability-bound identity that would make it a real boundary is the
//! durable-identity follow-up (issue #49).

use base64::Engine as _;

use super::responses::{encode_error, encode_success};
use crate::api::schema::{
    GramDeleteParams, GramDirection, GramFileInfo, GramFileUpload, GramGetFileChunkParams,
    GramGetFileParams, GramGrabParams, GramListParams, GramMarkReadParams, GramMessageInfo,
    GramPostParams, GramSendParams, GramUploadChunkParams, GramUploadDiscardParams,
    GramUploadStreamParams, ResponseResult,
};
#[cfg(unix)]
use crate::api::schema::{GramRelayCall, GramRelayParams};
use crate::app::App;
use crate::persist::gram::{
    new_id, GramDirection as StoredDirection, GramFile, GramItem, MAX_LABEL_BYTES, MAX_MIME_BYTES,
    MAX_TEXT_BYTES,
};

/// Ceiling on a `gram.list` page. A page is meant to be one screenful plus the
/// scroll ahead of it; 500 is far past that and still an order of magnitude under
/// the ~870-message store that made the unpaged answer slow. Clamping (rather than
/// rejecting) keeps a client that asks for too much working.
const GRAM_LIST_MAX_LIMIT: usize = 500;

/// Why a claim could not be completed.
enum GrabError {
    NotFound,
    /// The item is not a shared, still-open queue item (direct message, wrong
    /// direction, or already claimed by name below).
    NotGrabbable,
    /// Already claimed; carries the current grabber's identity.
    AlreadyGrabbed(String),
}

/// The result of a delete attempt, decided under the store lock.
enum DeleteOutcome {
    /// The message was removed. Carries its id so the handler can also delete any
    /// attached file bytes on disk once file attachments exist.
    Deleted(String),
    /// No message with that id.
    NotFound,
    /// The message exists but the calling agent is not involved in it.
    Forbidden,
}

/// Whether a Unix socket accepts a connection right now. The probe closes
/// without sending a request.
fn socket_accepts(path: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::net::UnixStream::connect(path).is_ok()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        false
    }
}

impl App {
    /// Resolve a peer's claimed pane only within that peer's pinned roster.
    /// Same-user processes on the trusted peer can claim another pane ID; this
    /// is an ordinary-caller boundary, not per-process isolation.
    #[cfg(unix)]
    fn relay_identity(&self, alias: &str, pane: Option<&str>) -> Option<String> {
        let pane = pane?.trim();
        if pane.is_empty() || pane.contains('/') {
            return None;
        }
        let qualified = format!("{alias}/{pane}");
        self.federation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .merged_agents()
            .into_iter()
            .find(|agent| {
                agent.pane_id == qualified
                    && agent.reachability
                        == Some(crate::api::federation_store::Reachability::Reachable)
            })
            .map(|agent| agent.name.unwrap_or(qualified))
    }

    /// Effective relay policy for both roles plus live gateway and socket state.
    pub(super) fn handle_gram_relay_status(&self, id: String) -> String {
        let policy = crate::api::gram_relay::policy();
        let peers = self
            .federation_manager
            .as_ref()
            .map(|manager| manager.gram_relay_peer_statuses(&policy.allowed_peers()))
            .unwrap_or_default();
        encode_success(
            id,
            crate::api::schema::ResponseResult::GramRelayStatus {
                coordinator: policy.coordinator_status(peers),
                remote: policy.remote_status(socket_accepts),
            },
        )
    }

    #[cfg(unix)]
    pub(super) fn handle_gram_relay(&mut self, id: String, params: GramRelayParams) -> String {
        if self.no_session {
            return gram_unavailable(id);
        }
        let alias = params.peer_alias;
        if !crate::api::gram_relay::policy().allows(&alias) {
            return encode_error(id, "forbidden", "Gram relay is disabled for this peer");
        }
        match params.call {
            GramRelayCall::UploadChunk(mut chunk) => {
                chunk.upload_id = relay_upload_id(&alias, &chunk.upload_id);
                self.handle_gram_upload_chunk(id, chunk)
            }
            GramRelayCall::UploadDiscard(mut discard) => {
                discard.upload_id = relay_upload_id(&alias, &discard.upload_id);
                self.handle_gram_upload_discard(id, discard)
            }
            GramRelayCall::Send(mut send) => {
                let Some(from) = self.relay_identity(&alias, send.caller_pane_id.as_deref()) else {
                    return encode_error(
                        id,
                        "unknown_caller",
                        "pane does not belong to this machine's live agent roster",
                    );
                };
                if let Some(error) = validate_label(&id, "from", Some(&from)) {
                    return error;
                }
                let text = send.text.trim();
                if let Some(error) = validate_text(&id, text, send.file.is_some()) {
                    return error;
                }
                if let Some(file) = send.file.as_mut() {
                    if file.sha256.as_deref().is_none_or(|hash| {
                        hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit())
                    }) {
                        return encode_error(
                            id,
                            "invalid_params",
                            "remote attachment requires a source SHA-256",
                        );
                    }
                    file.upload_id = relay_upload_id(&alias, &file.upload_id);
                }
                // A transport retry with the same request id reads the durable
                // result rather than creating a duplicate or consuming staging
                // twice. The CLI mints a fresh id per new send.
                use sha2::{Digest as _, Sha256};
                let message_id = format!("relay-{:x}", Sha256::digest(format!("{alias}\\0{id}")));
                let store_id = crate::persist::machine::get_or_create();
                if let Some(item) = crate::persist::gram::load()
                    .into_iter()
                    .find(|item| item.id == message_id)
                {
                    let same_file = match (&item.file, &send.file) {
                        (Some(stored), Some(incoming)) => {
                            stored.name
                                == crate::persist::gram_files::safe_file_name(&incoming.name)
                                && incoming.sha256.as_deref() == Some(stored.sha256.as_str())
                                && stored.mime == incoming.mime
                        }
                        (None, None) => true,
                        _ => false,
                    };
                    if item.direction != StoredDirection::AgentToOwner
                        || item.from != from
                        || item.text != text
                        || !same_file
                    {
                        return encode_error(
                            id,
                            "idempotency_conflict",
                            "relay delivery id was already used for different content",
                        );
                    }
                    return encode_success(
                        id,
                        ResponseResult::GramSent {
                            message: gram_item_to_info(item, &self.gram_machine_labels()),
                            store_id,
                        },
                    );
                }
                let file = match attach_file(&id, &message_id, send.file) {
                    Ok(file) => file,
                    Err(error) => return error,
                };
                let item = GramItem {
                    expired_file: None,
                    id: message_id,
                    direction: StoredDirection::AgentToOwner,
                    from: from.clone(),
                    to: None,
                    text: text.to_owned(),
                    grabbed_by: None,
                    grabbed_unix_ms: None,
                    created_unix_ms: super::unix_millis_now(),
                    read_by_owner: false,
                    file,
                    origin_id: store_id.clone(),
                    // Relayed from another machine: never a local agent's Gram.
                    sender: None,
                };
                match crate::persist::gram::append(item.clone()) {
                    Ok(_) => {
                        self.emit_apns_gram_message(&item);
                        encode_success(
                            id,
                            ResponseResult::GramSent {
                                message: gram_item_to_info(item, &self.gram_machine_labels()),
                                store_id,
                            },
                        )
                    }
                    Err(error) => {
                        crate::persist::gram_files::remove_message_files(&item.id);
                        encode_error(id, "gram_store_save_failed", error.to_string())
                    }
                }
            }
            GramRelayCall::List(list) => {
                let Some(identity) = self.relay_identity(&alias, list.caller_pane_id.as_deref())
                else {
                    return encode_error(
                        id,
                        "unknown_caller",
                        "pane does not belong to this machine's live agent roster",
                    );
                };
                self.handle_gram_list_for(id, list, Some(&identity))
            }
            GramRelayCall::GetFileChunk(file) => {
                let Some(identity) = self.relay_identity(&alias, file.caller_pane_id.as_deref())
                else {
                    return encode_error(
                        id,
                        "unknown_caller",
                        "pane does not belong to this machine's live agent roster",
                    );
                };
                self.read_gram_file_chunk(id, &file.id, file.offset, Some(&identity))
            }
            GramRelayCall::Delete(delete) => {
                let Some(identity) = self.relay_identity(&alias, delete.caller_pane_id.as_deref())
                else {
                    return encode_error(
                        id,
                        "unknown_caller",
                        "pane does not belong to this machine's live agent roster",
                    );
                };
                self.handle_gram_delete_for(id, delete.id, Some(identity))
            }
            GramRelayCall::Post(post) => {
                // Only a HerdrUp guest's post, and only to an agent of the
                // relaying machine: that machine serves the agent's guest. It
                // names the agent as it knows it; here that agent is
                // `<alias>/<name>`, as the federation roster qualifies it and
                // as the agent's own relayed list and file reads identify it.
                let to = post.to.trim();
                let qualified = format!("{alias}/{to}");
                if !crate::guest::store::valid_guest_name(&post.guest)
                    || to.is_empty()
                    || to.contains('/')
                    || !self.relay_agent_named(&alias, &qualified)
                {
                    return encode_error(
                        id,
                        "forbidden",
                        "a relayed post comes from a guest to one of this machine's agents",
                    );
                }
                let file = post.file.map(|mut file| {
                    file.upload_id = relay_upload_id(&alias, &file.upload_id);
                    file
                });
                let params = GramPostParams {
                    text: post.text,
                    to: Some(qualified),
                    file,
                    from: Some(crate::guest::post_from(&post.guest)),
                };
                self.store_gram_post(id, params, false)
            }
        }
    }

    /// Whether a reachable agent of peer `alias` has this qualified name.
    #[cfg(unix)]
    fn relay_agent_named(&self, alias: &str, name: &str) -> bool {
        let prefix = format!("{alias}/");
        !name.is_empty()
            && self
                .federation
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .merged_agents()
                .into_iter()
                .any(|agent| {
                    agent.pane_id.starts_with(&prefix)
                        && agent.name.as_deref() == Some(name)
                        && agent.reachability
                            == Some(crate::api::federation_store::Reachability::Reachable)
                })
    }

    pub(super) fn handle_gram_send(&mut self, id: String, params: GramSendParams) -> String {
        let text = params.text.trim();
        // A file-only message (no caption) is fine; an empty text-only message is
        // not.
        if let Some(err) = validate_text(&id, text, params.file.is_some()) {
            return err;
        }
        if let Some(err) = validate_label(&id, "from", params.from.as_deref()) {
            return err;
        }
        if self.no_session {
            return gram_unavailable(id);
        }

        let from = self.resolve_sender(params.from.as_deref(), params.caller_pane_id.as_deref());
        let message_id = new_id();
        let store_id = crate::persist::machine::get_or_create();
        let file = match attach_file(&id, &message_id, params.file) {
            Ok(file) => file,
            Err(err) => return err,
        };
        let sender = params
            .caller_pane_id
            .as_deref()
            .and_then(|pane| self.caller_sender(pane));
        let item = GramItem {
            expired_file: None,
            id: message_id,
            direction: StoredDirection::AgentToOwner,
            from: from.clone(),
            to: None,
            text: text.to_string(),
            grabbed_by: None,
            grabbed_unix_ms: None,
            created_unix_ms: super::unix_millis_now(),
            read_by_owner: false,
            file,
            origin_id: store_id.clone(),
            sender,
        };

        match crate::persist::gram::append(item.clone()) {
            Ok(_) => {
                self.emit_apns_gram_message(&item);
                encode_success(
                    id,
                    ResponseResult::GramSent {
                        message: gram_item_to_info(item, &self.gram_machine_labels()),
                        store_id,
                    },
                )
            }
            Err(err) => {
                // The record didn't persist; don't leave orphaned attachment bytes.
                crate::persist::gram_files::remove_message_files(&item.id);
                encode_error(id, "gram_store_save_failed", err.to_string())
            }
        }
    }

    pub(super) fn handle_gram_post(&mut self, id: String, params: GramPostParams) -> String {
        self.store_gram_post(id, params, true)
    }

    /// Store an owner-to-agent post. `check_live` refuses a `to` that names no
    /// live agent here; a relayed post names an agent of the relaying machine
    /// instead, checked by the caller.
    fn store_gram_post(&mut self, id: String, params: GramPostParams, check_live: bool) -> String {
        let text = params.text.trim();
        if let Some(err) = validate_text(&id, text, params.file.is_some()) {
            return err;
        }
        if let Some(err) = validate_label(&id, "to", params.to.as_deref()) {
            return err;
        }
        if self.no_session {
            return gram_unavailable(id);
        }

        let to = params
            .to
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        // A direct message must name a live agent, else it would be visible to no
        // one and never expire — a silent black hole. Omit `to` for the shared
        // queue instead.
        if let Some(target) = to.as_ref().filter(|_| check_live) {
            if !self.is_live_agent_name(target) {
                return encode_error(
                    id,
                    "invalid_params",
                    format!(
                        "no live agent named '{target}'; omit --to to post to the shared queue"
                    ),
                );
            }
        }

        let message_id = new_id();
        let store_id = crate::persist::machine::get_or_create();
        let file = match attach_file(&id, &message_id, params.file) {
            Ok(file) => file,
            Err(err) => return err,
        };
        let item = GramItem {
            expired_file: None,
            id: message_id,
            direction: StoredDirection::OwnerToAgent,
            from: params.from.unwrap_or_else(|| "owner".to_string()),
            to,
            text: text.to_string(),
            grabbed_by: None,
            grabbed_unix_ms: None,
            created_unix_ms: super::unix_millis_now(),
            // The owner's own message is not an unread inbox item for the owner.
            read_by_owner: true,
            file,
            origin_id: store_id.clone(),
            sender: None,
        };

        match crate::persist::gram::append(item.clone()) {
            Ok(_) => encode_success(
                id,
                ResponseResult::GramSent {
                    // An owner post names no sender machine.
                    message: gram_item_to_info(item, &MachineLabels::new()),
                    store_id,
                },
            ),
            Err(err) => {
                crate::persist::gram_files::remove_message_files(&item.id);
                encode_error(id, "gram_store_save_failed", err.to_string())
            }
        }
    }

    pub(super) fn handle_gram_list(&mut self, id: String, params: GramListParams) -> String {
        self.handle_gram_list_for(id, params, None)
    }

    fn handle_gram_list_for(
        &mut self,
        id: String,
        params: GramListParams,
        forced_identity: Option<&str>,
    ) -> String {
        if self.no_session {
            return gram_unavailable(id);
        }
        let limit = match params.limit {
            // An explicit zero would answer "nothing here" for a store that is not
            // empty, which a scrolling reader cannot tell from the end of the list.
            // A client asking for no messages is a bug worth surfacing.
            Some(0) => {
                return encode_error(
                    id,
                    "invalid_params",
                    "limit must be greater than zero; omit it to read the whole list",
                )
            }
            // Clamped rather than rejected: an over-eager client still gets a valid,
            // bounded page instead of an error it cannot act on.
            Some(requested) => Some(requested.min(GRAM_LIST_MAX_LIMIT)),
            None => None,
        };

        let items = crate::persist::gram::load();
        let filtered = match (forced_identity, params.caller_pane_id.as_deref()) {
            // A supplied caller pane selects the agent view. Failing open to the
            // owner view (as an earlier version did) would silently drop
            // `only_queue` and return a state-dependent answer; mirror
            // `pane.current`'s pane_not_found instead. (Not a confidentiality
            // boundary — the owner view is reachable by omitting the pane.)
            (None, Some(pane)) => {
                // `unread_only` is an owner-view filter with no meaning here; reject
                // the combination rather than silently ignore it.
                if params.unread_only {
                    return encode_error(
                        id,
                        "invalid_params",
                        "unread_only is only valid in the owner view; omit caller_pane_id",
                    );
                }
                let Some(identity) = self.caller_identity(pane) else {
                    return encode_error(
                        id,
                        "unknown_caller",
                        "caller_pane_id is not a known pane; omit it to read as the owner",
                    );
                };
                filter_agent_view(&items, &identity, params.only_queue)
            }
            (None, None) => filter_owner_view(&items, params.only_queue, params.unread_only),
            (Some(identity), Some(_)) => {
                if params.unread_only {
                    return encode_error(
                        id,
                        "invalid_params",
                        "unread_only is only valid in the owner view",
                    );
                }
                filter_agent_view(&items, identity, params.only_queue)
            }
            (Some(_), None) => {
                return encode_error(id, "forbidden", "a remote caller must identify its pane")
            }
        };
        // Counted over the whole filtered list, BEFORE paging: the badge and the
        // Read-all affordance describe the inbox, not the window the client happens
        // to be holding.
        let unread_count = filtered.iter().filter(|item| is_unread(item)).count();
        let labels = self.gram_machine_labels();
        // Store order is oldest-first; clients want newest-first.
        let mut messages: Vec<GramMessageInfo> = filtered
            .into_iter()
            .rev()
            .map(|item| gram_item_to_info(item, &labels))
            .collect();
        let store_id = crate::persist::machine::get_or_create();
        // Over the FULL list, not the page, so a paging client can keep polling the
        // head for a few hundred bytes.
        let digest = list_digest(&store_id, &messages);
        // Conditional fetch. The digest covers the store id as well as the messages, so
        // a client that has been pointed at a DIFFERENT store can never be told
        // "unchanged" while holding another store's list.
        //
        // Head-only: a request with `before_id` asks for an older page the client does
        // not hold yet, so answering "unchanged" would starve its scroll.
        if params.before_id.is_none()
            && params.if_unchanged_digest.as_deref() == Some(digest.as_str())
        {
            return encode_success(id, ResponseResult::GramListUnchanged { store_id, digest });
        }
        // Paging happens after the audience filter and after the reverse, so a cursor
        // means the same thing — "the message after this one, going older" — in the
        // owner view and the agent view alike.
        if let Some(cursor) = params.before_id.as_deref() {
            match messages.iter().position(|message| message.id == cursor) {
                Some(index) => {
                    messages.drain(..=index);
                }
                // Never fall back to the head: a client whose cursor aged out of the
                // list (deleted message, switched filter) would otherwise be handed
                // page 1 again on every scroll, forever.
                None => return encode_error(id, "invalid_params", "before_id is not in this list"),
            }
        }
        let has_more = limit.is_some_and(|limit| messages.len() > limit);
        if let Some(limit) = limit {
            messages.truncate(limit);
        }
        encode_success(
            id,
            ResponseResult::GramList {
                messages,
                store_id,
                digest,
                has_more,
                unread_count,
            },
        )
    }

    pub(super) fn handle_gram_grab(&mut self, id: String, params: GramGrabParams) -> String {
        if let Some(err) = validate_label(&id, "grabbed_by", params.grabbed_by.as_deref()) {
            return err;
        }
        if self.no_session {
            return gram_unavailable(id);
        }

        // Claimant = explicit --as override, else the caller pane's identity.
        let who = params
            .grabbed_by
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .or_else(|| {
                params
                    .caller_pane_id
                    .as_deref()
                    .and_then(|pane| self.caller_identity(pane))
            });
        let Some(who) = who else {
            return encode_error(
                id,
                "unknown_caller",
                "could not resolve the grabbing agent; pass a valid caller_pane_id or grabbed_by",
            );
        };

        let target_id = params.id.clone();
        let now = super::unix_millis_now();
        // The claim runs under the store's advisory lock, and the app loop
        // serializes API requests, so this check-then-set is atomic across both
        // threads and processes — first grab wins, independent of identity. A lost
        // race changes nothing, so it does not rewrite the store.
        let outcome = crate::persist::gram::update_if_changed(move |items| {
            let result = (|| {
                let Some(item) = items.iter_mut().find(|item| item.id == target_id) else {
                    return Err(GrabError::NotFound);
                };
                if item.direction != StoredDirection::OwnerToAgent || item.to.is_some() {
                    return Err(GrabError::NotGrabbable);
                }
                if let Some(existing) = &item.grabbed_by {
                    return Err(GrabError::AlreadyGrabbed(existing.clone()));
                }
                item.grabbed_by = Some(who.clone());
                item.grabbed_unix_ms = Some(now);
                Ok(item.clone())
            })();
            let changed = result.is_ok();
            (result, changed)
        });

        match outcome {
            Ok((Ok(item), _)) => encode_success(
                id,
                ResponseResult::GramGrabbed {
                    // A claimed queue item is an owner post: no sender machine.
                    message: gram_item_to_info(item, &MachineLabels::new()),
                },
            ),
            Ok((Err(GrabError::NotFound), _)) => {
                encode_error(id, "not_found", "no gram message with that id")
            }
            Ok((Err(GrabError::NotGrabbable), _)) => encode_error(
                id,
                "not_grabbable",
                "that message is not a shared-queue item",
            ),
            Ok((Err(GrabError::AlreadyGrabbed(owner)), _)) => {
                encode_error(id, "already_grabbed", format!("already grabbed by {owner}"))
            }
            Err(err) => encode_error(id, "gram_store_save_failed", err.to_string()),
        }
    }

    pub(super) fn handle_gram_mark_read(
        &mut self,
        id: String,
        params: GramMarkReadParams,
    ) -> String {
        if self.no_session {
            return gram_unavailable(id);
        }

        let targets: Vec<String> = params.targets().map(str::to_string).collect();
        if targets.is_empty() {
            return encode_error(id, "invalid_params", "pass id or ids");
        }
        // Returns (found, changed): every id must exist before any is marked,
        // and a re-mark of already-read messages does not rewrite the store.
        let outcome = crate::persist::gram::update_if_changed(move |items| {
            if !targets
                .iter()
                .all(|target| items.iter().any(|item| &item.id == target))
            {
                return (false, false);
            }
            let mut changed = false;
            for item in items.iter_mut().filter(|item| targets.contains(&item.id)) {
                changed |= !item.read_by_owner;
                item.read_by_owner = true;
            }
            (true, changed)
        });
        match outcome {
            Ok((true, _)) => encode_success(id, ResponseResult::Ok {}),
            Ok((false, _)) => encode_error(id, "not_found", "no gram message with that id"),
            Err(err) => encode_error(id, "gram_store_save_failed", err.to_string()),
        }
    }

    pub(super) fn handle_gram_delete(&mut self, id: String, params: GramDeleteParams) -> String {
        if self.no_session {
            return gram_unavailable(id);
        }

        // Resolve the caller's authority. The owner's app sends no caller pane and
        // may delete anything; an agent supplies its pane and may delete only a
        // message it is involved in. A caller pane that names no live pane is an
        // error, mirroring `gram.list` — not a silent fall-through to owner power.
        let identity = match params.caller_pane_id.as_deref() {
            Some(pane) => match self.caller_identity(pane) {
                Some(identity) => Some(identity),
                None => {
                    return encode_error(
                        id,
                        "unknown_caller",
                        "caller_pane_id is not a known pane; omit it to delete as the owner",
                    );
                }
            },
            None => None,
        };

        self.handle_gram_delete_for(id, params.id, identity)
    }

    fn handle_gram_delete_for(
        &mut self,
        id: String,
        target_id: String,
        identity: Option<String>,
    ) -> String {
        let outcome = crate::persist::gram::update_if_changed(move |items| {
            apply_delete(items, &target_id, identity.as_deref())
        });

        match outcome {
            Ok((DeleteOutcome::Deleted(removed_id), _)) => {
                // Remove the attachment bytes too, so a secret (a temporary API key
                // sent as a file) does not outlive the record it was deleted with.
                crate::persist::gram_files::remove_message_files(&removed_id);
                encode_success(id, ResponseResult::Ok {})
            }
            Ok((DeleteOutcome::NotFound, _)) => {
                encode_error(id, "not_found", "no gram message with that id")
            }
            Ok((DeleteOutcome::Forbidden, _)) => encode_error(
                id,
                "forbidden",
                "you can only delete a gram message you sent, grabbed, or that is addressed to you",
            ),
            Err(err) => encode_error(id, "gram_store_save_failed", err.to_string()),
        }
    }

    pub(super) fn handle_gram_upload_chunk(
        &mut self,
        id: String,
        params: GramUploadChunkParams,
    ) -> String {
        if self.no_session {
            return gram_unavailable(id);
        }
        // Single writer per upload_id, and the CLAIM is the check: a predicate read
        // before appending would leave the window open, since a stream can open
        // between the read and the write. A live `gram.upload.stream` channel appends
        // on the server thread with no lock, and an `offset: 0` chunk here would
        // TRUNCATE the staging file, discarding bytes that channel already acked. The
        // offset rule would make that loud rather than silent, but a second writer on
        // one upload is always a client bug: refuse it. Held only for this append.
        let Some(_claim) = crate::api::UploadClaim::acquire(&params.upload_id) else {
            return encode_error(
                id,
                "upload_in_progress",
                "another writer owns this upload_id",
            );
        };
        let bytes = match base64::engine::general_purpose::STANDARD
            .decode(params.data_base64.as_bytes())
        {
            Ok(bytes) => bytes,
            Err(_) => return encode_error(id, "invalid_params", "data_base64 is not valid base64"),
        };
        match crate::persist::gram_files::append_chunk(&params.upload_id, params.offset, &bytes) {
            Ok(()) => encode_success(id, ResponseResult::Ok {}),
            Err(err) if err.kind() == std::io::ErrorKind::InvalidInput => {
                encode_error(id, "invalid_params", err.to_string())
            }
            Err(err) => encode_error(id, "gram_file_error", err.to_string()),
        }
    }

    /// `gram.upload.discard`: delete a staged upload the client will never attach,
    /// freeing its share of the staging budget now rather than at the 24 h sweep.
    /// Authority is the id itself: the owner may discard any staged upload, while a
    /// guest's and a relayed peer's ids are rewritten into their own namespace
    /// before they reach here, so neither can name another caller's upload.
    pub(super) fn handle_gram_upload_discard(
        &mut self,
        id: String,
        params: GramUploadDiscardParams,
    ) -> String {
        if self.no_session {
            return gram_unavailable(id);
        }
        // Deleting is a fourth writer on the staging file: a live stream's next
        // frame would fail its offset check against the vanished bytes, and a
        // finalize between its hash and its rename would fail with the bytes gone.
        // Refuse while any writer holds the id, exactly as `gram.upload_chunk` and
        // attach do.
        let Some(_claim) = crate::api::UploadClaim::acquire(&params.upload_id) else {
            return encode_error(
                id,
                "upload_in_progress",
                "another writer owns this upload_id; read the upload connection to EOF before discarding",
            );
        };
        match crate::persist::gram_files::discard_staged(&params.upload_id) {
            Ok(discarded) => encode_success(id, ResponseResult::GramUploadDiscarded { discarded }),
            Err(err) => encode_error(id, "gram_file_error", err.to_string()),
        }
    }

    /// Validates a streaming upload before the server thread starts reading frames.
    /// `no_session` is the ONLY app-owned state the per-chunk handler consults; every
    /// other step (base64 decode, `append_chunk`) is pure filesystem and runs on the
    /// server thread, so this is the whole app-side cost of a streamed upload.
    pub(super) fn handle_gram_upload_stream_open(
        &mut self,
        id: String,
        _params: GramUploadStreamParams,
    ) -> String {
        if self.no_session {
            return gram_unavailable(id);
        }
        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_gram_get_file(&mut self, id: String, params: GramGetFileParams) -> String {
        if self.no_session {
            return gram_unavailable(id);
        }
        // Resolve the caller's authority: the owner (no caller pane) may download
        // any file; an agent may download only a file on a message it can see.
        let identity = match params.caller_pane_id.as_deref() {
            Some(pane) => match self.caller_identity(pane) {
                Some(identity) => Some(identity),
                None => {
                    return encode_error(
                        id,
                        "unknown_caller",
                        "caller_pane_id is not a known pane; omit it to read as the owner",
                    );
                }
            },
            None => None,
        };
        let Some(item) = crate::persist::gram::load()
            .into_iter()
            .find(|item| item.id == params.id)
        else {
            return encode_error(id, "not_found", "no gram message with that id");
        };
        if let Some(identity) = &identity {
            if !agent_can_see(&item, identity) {
                return encode_error(
                    id,
                    "forbidden",
                    "you can only download a file on a message you can see",
                );
            }
        }
        let Some(file) = item.file else {
            return encode_error(id, "no_file", "that message has no attached file");
        };
        match crate::persist::gram_files::read_message_file(&item.id, &file.name) {
            Ok(bytes) => {
                let data_base64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                encode_success(
                    id,
                    ResponseResult::GramFileContent {
                        name: file.name,
                        mime: file.mime,
                        size: file.size,
                        data_base64,
                    },
                )
            }
            Err(err) => encode_error(id, "gram_file_error", format!("failed to read file: {err}")),
        }
    }
    pub(super) fn handle_gram_get_file_chunk(
        &mut self,
        id: String,
        params: GramGetFileChunkParams,
    ) -> String {
        if self.no_session {
            return gram_unavailable(id);
        }
        let identity = match params.caller_pane_id.as_deref() {
            Some(pane) => match self.caller_identity(pane) {
                Some(identity) => Some(identity),
                None => {
                    return encode_error(id, "unknown_caller", "caller_pane_id is not a known pane")
                }
            },
            None => None,
        };
        self.read_gram_file_chunk(id, &params.id, params.offset, identity.as_deref())
    }

    fn read_gram_file_chunk(
        &self,
        id: String,
        message_id: &str,
        offset: u64,
        identity: Option<&str>,
    ) -> String {
        let Some(item) = crate::persist::gram::load()
            .into_iter()
            .find(|item| item.id == message_id)
        else {
            return encode_error(id, "not_found", "no gram message with that id");
        };
        if identity.is_some_and(|identity| !agent_can_see(&item, identity)) {
            return encode_error(
                id,
                "forbidden",
                "you can only download a file on a message you can see",
            );
        }
        let Some(file) = item.file else {
            return encode_error(id, "no_file", "that message has no attached file");
        };
        if offset > file.size {
            return encode_error(id, "invalid_params", "file offset exceeds size");
        }
        match crate::persist::gram_files::read_message_file_chunk(
            &item.id,
            &file.name,
            offset,
            crate::persist::gram_files::MAX_CHUNK_BYTES as u64,
        ) {
            Ok(bytes) => encode_success(
                id,
                ResponseResult::GramFileChunk {
                    name: file.name,
                    mime: file.mime,
                    size: file.size,
                    sha256: file.sha256,
                    offset,
                    data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
                },
            ),
            Err(err) => encode_error(id, "gram_file_error", format!("failed to read file: {err}")),
        }
    }

    /// Resolve the label to record as `from` for an agent->owner message: an
    /// explicit override, else the caller pane's identity, else "agent". An
    /// explicit `from` overrides attribution entirely (the message is then
    /// attributed to that label, not the caller), which the CLI help notes.
    fn resolve_sender(&self, from: Option<&str>, caller_pane_id: Option<&str>) -> String {
        from.map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .or_else(|| caller_pane_id.and_then(|pane| self.caller_identity(pane)))
            .unwrap_or_else(|| "agent".to_string())
    }

    /// Resolve a public pane id (an agent's `HERDR_PANE_ID`) to its identity: the
    /// per-agent name if set (durable across restart / live-handoff, since it is
    /// snapshotted and restored), else the pane's public id. `None` only when the
    /// pane id names no known pane. See the module header for the name-semantics
    /// this identity carries.
    fn caller_identity(&self, caller_pane_id: &str) -> Option<String> {
        let (ws_idx, pane_id) = self.parse_pane_id(caller_pane_id)?;
        let terminal_id = self.state.workspaces.get(ws_idx)?.terminal_id(pane_id)?;
        self.state
            .terminals
            .get(terminal_id)
            .and_then(|terminal| terminal.agent_name.clone())
            .filter(|name| !name.trim().is_empty())
            .or_else(|| self.public_pane_id(ws_idx, pane_id))
    }

    /// The terminal and agent kind behind a caller pane, recorded on the Gram
    /// it sends so a guest's view follows the agent, not a reusable name.
    fn caller_sender(&self, caller_pane_id: &str) -> Option<crate::persist::gram::GramSender> {
        let (ws_idx, pane_id) = self.parse_pane_id(caller_pane_id)?;
        let terminal_id = self.state.workspaces.get(ws_idx)?.terminal_id(pane_id)?;
        let terminal = self.state.terminals.get(terminal_id)?;
        Some(crate::persist::gram::GramSender {
            terminal_id: terminal_id.to_string(),
            agent: terminal.effective_agent_label().map(str::to_string),
        })
    }

    /// Whether some live terminal has this exact unique agent name. Used to reject
    /// a direct `gram.post` to a nonexistent agent instead of black-holing it.
    fn is_live_agent_name(&self, name: &str) -> bool {
        self.state
            .terminals
            .values()
            .any(|terminal| terminal.agent_name.as_deref() == Some(name))
            || {
                #[cfg(unix)]
                {
                    name.contains('/')
                        && self
                            .federation
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .merged_agents()
                            .iter()
                            .any(|agent| {
                                agent.name.as_deref() == Some(name)
                                    && agent.reachability
                                        == Some(
                                            crate::api::federation_store::Reachability::Reachable,
                                        )
                                    && agent.machine_id.as_deref().is_some_and(|alias| {
                                        crate::api::gram_relay::policy().allows(alias)
                                    })
                            })
                }
                #[cfg(not(unix))]
                {
                    false
                }
            }
    }

    /// Deliver one gram alert to registered devices that opted into gram push.
    /// A sibling of `emit_apns_agent_notifications`: detached, best-effort, guarded
    /// by `crate::push::may_deliver`. The alert deep-links to the app's Gram page, so
    /// it carries no pane/workspace id (the payload's `gram` marker signals this).
    fn emit_apns_gram_message(&self, item: &GramItem) {
        if self.no_session || !crate::push::may_deliver(&self.state.push_config) {
            return;
        }
        let labels = self.gram_machine_labels();
        crate::push::dispatch(
            self.state.push_config.clone(),
            vec![gram_push_notification(
                item,
                sender_machine_label(item, &labels),
            )],
        );
    }

    /// Display label of each federated machine by routing alias: the label the
    /// federation store stamps on that machine's cached agents and workspaces.
    /// A machine whose label is only its alias has none.
    fn gram_machine_labels(&self) -> MachineLabels {
        self.federation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .peers()
            .filter_map(|(alias, entry)| {
                entry
                    .agents
                    .iter()
                    .filter_map(|agent| agent.machine_label.as_deref())
                    .chain(
                        entry
                            .workspaces
                            .iter()
                            .filter_map(|workspace| workspace.machine_label.as_deref()),
                    )
                    .map(str::trim)
                    .find(|label| !label.is_empty() && *label != alias)
                    .map(|label| (alias.to_owned(), label.to_owned()))
            })
            .collect()
    }
}

/// Federated machines' display labels by routing alias.
type MachineLabels = std::collections::HashMap<String, String>;

/// The label of the federated machine a relayed Gram came from: `from` is
/// `<alias>/<name>` and `alias` has a label. `None` for a local Gram, an
/// owner post, or an alias without a label.
fn sender_machine_label<'a>(item: &GramItem, labels: &'a MachineLabels) -> Option<&'a str> {
    if item.direction != StoredDirection::AgentToOwner {
        return None;
    }
    let (alias, name) = item.from.split_once('/')?;
    if name.is_empty() {
        return None;
    }
    labels.get(alias).map(String::as_str)
}

/// The alert for a new Gram to the owner. It deep-links to the app's Gram
/// page, so it carries no pane or workspace id. A Gram from a labeled
/// federated machine names it the way remote agent alerts do:
/// "llm-opt on Jerry's Mac Studio".
pub(crate) fn gram_push_notification(
    item: &GramItem,
    machine_label: Option<&str>,
) -> crate::push::PushNotification {
    let sender = match machine_label {
        Some(label) => {
            let name = item
                .from
                .split_once('/')
                .map_or(item.from.as_str(), |(_, name)| name);
            format!("{name} on {label}")
        }
        None => item.from.clone(),
    };
    let title =
        super::sanitized_notification_text(&sender, 80).unwrap_or_else(|| "New gram".to_string());
    let mut body = super::sanitized_notification_text(&item.text, 240).unwrap_or_default();
    // Note an attachment so a file-only (or captioned) gram reads sensibly on
    // the lock screen. The name is already a sanitized basename.
    if let Some(file) = &item.file {
        let hint = format!("📎 {}", file.name);
        body = if body.is_empty() {
            hint
        } else {
            format!("{body}\n{hint}")
        };
    }
    crate::push::PushNotification {
        title,
        body,
        pane_id: String::new(),
        workspace_id: String::new(),
        kind: crate::push::PushKind::Gram,
        #[cfg(unix)]
        guest_scope: Some(crate::guest::push::GuestScope::Gram {
            from: item.from.clone(),
            sender: item.sender.clone(),
            gram_id: item.id.clone(),
        }),
        ask_source: None,
        ask: None,
    }
}

/// A peer cannot collide with another peer's staging upload, even by reusing
/// the same client-chosen upload id.
#[cfg(unix)]
fn relay_upload_id(alias: &str, upload_id: &str) -> String {
    use sha2::{Digest as _, Sha256};
    format!(
        "relay-{:x}",
        Sha256::digest(format!("{alias}\\0{upload_id}"))
    )
}

/// Reject an over-long message, and an empty one unless a file is attached (a
/// file with no caption is fine). Returns the encoded error response, or `None`
/// when the text is acceptable.
fn validate_text(id: &str, text: &str, allow_empty: bool) -> Option<String> {
    if text.is_empty() && !allow_empty {
        return Some(encode_error(
            id.to_string(),
            "invalid_params",
            "text is empty",
        ));
    }
    if text.len() > MAX_TEXT_BYTES {
        return Some(encode_error(
            id.to_string(),
            "invalid_params",
            format!("text exceeds {MAX_TEXT_BYTES} bytes; send large content as a file"),
        ));
    }
    None
}

/// Assemble a staged upload onto `message_id`, returning its metadata for the
/// record. No file → `Ok(None)`. A bad upload (missing/oversized/invalid id or
/// name) returns an encoded error response so the caller can return it directly.
fn attach_file(
    request_id: &str,
    message_id: &str,
    upload: Option<GramFileUpload>,
) -> Result<Option<GramFile>, String> {
    let Some(upload) = upload else {
        return Ok(None);
    };
    if upload.name.trim().is_empty() {
        return Err(encode_error(
            request_id.to_string(),
            "invalid_params",
            "file.name is empty",
        ));
    }
    // The mime is caller-supplied and persisted in gram.json, which the store's
    // byte budget does NOT count (it budgets text). Cap it so a caller cannot
    // smuggle large data through this field and bloat the store past its budget.
    if let Some(err) = validate_mime(request_id, &upload.mime) {
        return Err(err);
    }
    // Finalize is the THIRD writer on a staging file, and it is no longer serialized
    // against appends: before streaming, every chunk ran on this single-threaded app
    // loop, so a finalize could not overlap one. Now appends run on the API server
    // thread, and `finalize` reads the size, hashes the file, then renames it — so a
    // frame landing between the size read and the hash records a sha256 taken over
    // MORE bytes than the recorded size. That is silent corruption of the integrity
    // fields a client verifies a download against, and it is the hazard this lock
    // exists for.
    //
    // A frame arriving after the RENAME is NOT part of it: staging and message paths
    // are `gram-files/.staging/<upload_id>` and `gram-files/<message_id>/<name>`, and
    // the second is not derivable from an upload_id, so a late append creates a fresh
    // orphaned staging file rather than writing into the attachment. Do not widen or
    // narrow this lock on the strength of that; the size/hash window is the reason.
    //
    // The claim is HELD ACROSS the whole sequence, not merely consulted before it: a
    // check that releases the lock and then finalizes still admits a stream that
    // opens in between, which is the same corruption with a narrower window.
    let Some(_claim) = crate::api::UploadClaim::acquire(&upload.upload_id) else {
        return Err(encode_error(
            request_id.to_string(),
            "upload_in_progress",
            // Names the actual wait condition. A client that merely closed its write
            // half has NOT waited: the claim lives until the daemon's serve thread
            // observes that EOF, and it is released before the socket, so reading the
            // upload connection to EOF is the synchronization point.
            "another writer owns this upload_id; read the upload connection to EOF before attaching",
        ));
    };
    match crate::persist::gram_files::finalize(message_id, &upload.upload_id, &upload.name) {
        Ok(finalized) => {
            if upload
                .sha256
                .as_deref()
                .is_some_and(|hash| hash != finalized.sha256.as_str())
            {
                crate::persist::gram_files::remove_message_files(message_id);
                return Err(encode_error(
                    request_id.to_string(),
                    "hash_mismatch",
                    "uploaded bytes do not match the source SHA-256",
                ));
            }
            Ok(Some(GramFile {
                name: finalized.name,
                size: finalized.size,
                mime: upload.mime,
                sha256: finalized.sha256,
            }))
        }
        // A malformed upload (unknown id, empty or oversized staging, bad name) is
        // the caller's mistake; anything else is a real I/O failure.
        Err(err) if err.kind() == std::io::ErrorKind::InvalidInput => Err(encode_error(
            request_id.to_string(),
            "invalid_params",
            err.to_string(),
        )),
        Err(err) => Err(encode_error(
            request_id.to_string(),
            "gram_file_error",
            err.to_string(),
        )),
    }
}

/// Reject a persisted `mime` longer than [`MAX_MIME_BYTES`], so a caller cannot
/// smuggle large data through the one attachment field the store's text budget
/// does not count. Separate from [`validate_label`] because mime has its own,
/// larger bound.
fn validate_mime(id: &str, mime: &str) -> Option<String> {
    if mime.len() > MAX_MIME_BYTES {
        return Some(encode_error(
            id.to_string(),
            "invalid_params",
            format!("file.mime exceeds {MAX_MIME_BYTES} bytes"),
        ));
    }
    None
}

/// Reject a persisted label override (`from`, `to`, `grabbed_by`) longer than
/// [`MAX_LABEL_BYTES`], so a caller cannot bypass the text budget through them.
/// Measured after trimming, matching what the handlers persist.
fn validate_label(id: &str, field: &str, value: Option<&str>) -> Option<String> {
    match value.map(str::trim) {
        Some(value) if value.len() > MAX_LABEL_BYTES => Some(encode_error(
            id.to_string(),
            "invalid_params",
            format!("{field} exceeds {MAX_LABEL_BYTES} bytes"),
        )),
        _ => None,
    }
}

fn gram_unavailable(id: String) -> String {
    encode_error(
        id,
        "gram_unavailable",
        "gram requires the shared herdr server",
    )
}

fn gram_item_to_info(item: GramItem, labels: &MachineLabels) -> GramMessageInfo {
    let machine_label = sender_machine_label(&item, labels).map(str::to_owned);
    GramMessageInfo {
        id: item.id,
        direction: match item.direction {
            StoredDirection::AgentToOwner => GramDirection::AgentToOwner,
            StoredDirection::OwnerToAgent => GramDirection::OwnerToAgent,
        },
        from: item.from,
        to: item.to,
        text: item.text,
        grabbed_by: item.grabbed_by,
        grabbed_unix_ms: item.grabbed_unix_ms,
        created_unix_ms: item.created_unix_ms,
        read_by_owner: item.read_by_owner,
        file: item.file.map(|file| GramFileInfo {
            name: file.name,
            size: file.size,
            mime: file.mime,
            sha256: file.sha256,
        }),
        expired_file: item.expired_file.map(|file| GramFileInfo {
            name: file.name,
            size: file.size,
            mime: file.mime,
            sha256: file.sha256,
        }),
        origin_id: item.origin_id,
        machine_label,
    }
}

/// True for a shared, still-open queue item any agent may claim.
fn is_open_shared_queue(item: &GramItem) -> bool {
    item.direction == StoredDirection::OwnerToAgent
        && item.to.is_none()
        && item.grabbed_by.is_none()
}

/// Whether an agent identity may delete a message: it sent it, it is addressed to
/// it, or it grabbed it. The owner (no caller pane) bypasses this check entirely.
/// This is the same "involved in it" relation the agent view uses for membership,
/// minus the shared open queue — an agent should not be able to delete unclaimed
/// work it never touched out from under the owner.
fn agent_may_delete(item: &GramItem, identity: &str) -> bool {
    let sent_by_me = item.direction == StoredDirection::AgentToOwner && item.from == identity;
    let addressed_to_me =
        item.direction == StoredDirection::OwnerToAgent && item.to.as_deref() == Some(identity);
    let grabbed_by_me = item.grabbed_by.as_deref() == Some(identity);
    sent_by_me || addressed_to_me || grabbed_by_me
}

/// Decide and apply a delete against the in-memory list. `identity` is `None` for
/// the owner (may delete any message) or `Some(agent)` (may delete only a message
/// it is involved in). Returns the outcome plus whether the list changed, matching
/// [`crate::persist::gram::update_if_changed`]'s mutation contract — the store is
/// rewritten only on an actual removal. Pure over the list so the find/authorize/
/// remove logic is unit-tested without an App or the store.
fn apply_delete(
    items: &mut Vec<GramItem>,
    id: &str,
    identity: Option<&str>,
) -> (DeleteOutcome, bool) {
    let Some(pos) = items.iter().position(|item| item.id == id) else {
        return (DeleteOutcome::NotFound, false);
    };
    if let Some(identity) = identity {
        if !agent_may_delete(&items[pos], identity) {
            return (DeleteOutcome::Forbidden, false);
        }
    }
    let removed = items.remove(pos);
    (DeleteOutcome::Deleted(removed.id), true)
}

/// Whether a message belongs in an agent's view: the shared ungrabbed queue, an
/// item addressed to it, one it grabbed, or one it sent. This is the audience
/// boundary — an agent may list and download the files of what it can see, but not
/// another agent's direct message (which is how a secret is sent). The owner (no
/// caller pane) can see everything.
fn agent_can_see(item: &GramItem, identity: &str) -> bool {
    let addressed_to_me =
        item.direction == StoredDirection::OwnerToAgent && item.to.as_deref() == Some(identity);
    let grabbed_by_me = item.grabbed_by.as_deref() == Some(identity);
    let sent_by_me = item.direction == StoredDirection::AgentToOwner && item.from == identity;
    is_open_shared_queue(item) || addressed_to_me || grabbed_by_me || sent_by_me
}

/// The agent's view: the shared ungrabbed queue, items addressed to it, items it
/// grabbed, and its own sent messages. `only_queue` narrows to just the shared,
/// still-open queue so an agent can quickly scan available work. Membership is by
/// the caller's current identity (see the module header for the name-semantics).
fn filter_agent_view(items: &[GramItem], identity: &str, only_queue: bool) -> Vec<GramItem> {
    items
        .iter()
        .filter(|item| {
            if only_queue {
                return is_open_shared_queue(item);
            }
            agent_can_see(item, identity)
        })
        .cloned()
        .collect()
}

/// The owner's view: the shared open queue (`only_queue`), just unread
/// agent->owner messages (`unread_only`), or everything.
fn filter_owner_view(items: &[GramItem], only_queue: bool, unread_only: bool) -> Vec<GramItem> {
    items
        .iter()
        .filter(|item| {
            if only_queue {
                return is_open_shared_queue(item);
            }
            if unread_only {
                return is_unread(item);
            }
            true
        })
        .cloned()
        .collect()
}

/// An agent->owner message the owner has not read yet — the thing the app's badge
/// counts. Shared by the `unread_only` filter and the whole-list `unread_count`, so
/// the count can never drift from the filter.
fn is_unread(item: &GramItem) -> bool {
    item.direction == StoredDirection::AgentToOwner && !item.read_by_owner
}

/// Fingerprint of a `gram.list` answer, for conditional polling.
///
/// Hashed over the SERIALIZED payload rather than a hand-picked set of fields: the
/// digest then changes exactly when the reply would differ, and a new field on
/// `GramMessageInfo` cannot silently fall outside it. The store id is mixed in so a
/// client pointed at a different store is never told "unchanged" for a list it does
/// not hold.
fn list_digest(store_id: &str, messages: &[GramMessageInfo]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(store_id.as_bytes());
    hasher.update(b"\0");
    match serde_json::to_vec(messages) {
        Ok(bytes) => hasher.update(&bytes),
        // Cannot happen for these types, and must not be papered over with a
        // constant: mix in the error so the digest is at least unique per failure
        // rather than equal across unrelated answers.
        Err(err) => hasher.update(format!("serialize_error:{err}").as_bytes()),
    }
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner_shared(id: &str) -> GramItem {
        GramItem {
            expired_file: None,
            id: id.to_string(),
            direction: StoredDirection::OwnerToAgent,
            from: "owner".to_string(),
            to: None,
            text: "shared task".to_string(),
            grabbed_by: None,
            grabbed_unix_ms: None,
            created_unix_ms: 1,
            read_by_owner: true,
            file: None,
            origin_id: "machine_test".to_string(),
            sender: None,
        }
    }

    #[test]
    fn agent_view_shows_queue_direct_grabs_and_own_sends() {
        let mut direct = owner_shared("direct");
        direct.to = Some("alpha".to_string());
        let mut grabbed_by_me = owner_shared("mine");
        grabbed_by_me.grabbed_by = Some("alpha".to_string());
        let mut grabbed_by_other = owner_shared("theirs");
        grabbed_by_other.grabbed_by = Some("beta".to_string());
        let mut my_send = owner_shared("sent");
        my_send.direction = StoredDirection::AgentToOwner;
        my_send.from = "alpha".to_string();
        my_send.to = None;

        let items = vec![
            owner_shared("open"),
            direct,
            grabbed_by_me,
            grabbed_by_other,
            my_send,
        ];
        let ids: Vec<String> = filter_agent_view(&items, "alpha", false)
            .into_iter()
            .map(|item| item.id)
            .collect();
        assert!(ids.contains(&"open".to_string()));
        assert!(ids.contains(&"direct".to_string()));
        assert!(ids.contains(&"mine".to_string()));
        assert!(ids.contains(&"sent".to_string()));
        // A shared item grabbed by another agent is hidden.
        assert!(!ids.contains(&"theirs".to_string()));
    }

    #[test]
    fn agent_view_only_queue_is_shared_and_open() {
        let mut grabbed = owner_shared("grabbed");
        grabbed.grabbed_by = Some("beta".to_string());
        let items = vec![owner_shared("open"), grabbed];
        let ids: Vec<String> = filter_agent_view(&items, "alpha", true)
            .into_iter()
            .map(|item| item.id)
            .collect();
        assert_eq!(ids, vec!["open".to_string()]);
    }

    #[test]
    fn owner_view_default_unread_and_queue() {
        let mut unread = owner_shared("unread");
        unread.direction = StoredDirection::AgentToOwner;
        unread.read_by_owner = false;
        let mut read = owner_shared("read");
        read.direction = StoredDirection::AgentToOwner;
        read.read_by_owner = true;
        let mut grabbed = owner_shared("grabbed");
        grabbed.grabbed_by = Some("beta".to_string());

        let items = vec![owner_shared("open"), unread, read, grabbed];

        // Default: everything.
        assert_eq!(filter_owner_view(&items, false, false).len(), 4);
        // unread_only: just the unread agent->owner message.
        let unread_ids: Vec<String> = filter_owner_view(&items, false, true)
            .into_iter()
            .map(|item| item.id)
            .collect();
        assert_eq!(unread_ids, vec!["unread".to_string()]);
        // only_queue: just the shared, still-open item (not the grabbed one).
        let queue_ids: Vec<String> = filter_owner_view(&items, true, false)
            .into_iter()
            .map(|item| item.id)
            .collect();
        assert_eq!(queue_ids, vec!["open".to_string()]);
    }

    #[test]
    fn agent_may_delete_only_own_involvement() {
        // A shared, still-open queue item the agent never touched: not deletable
        // by an agent (only the owner may remove unclaimed work).
        assert!(!agent_may_delete(&owner_shared("open"), "alpha"));

        let mut direct = owner_shared("direct");
        direct.to = Some("alpha".to_string());
        assert!(agent_may_delete(&direct, "alpha"));
        assert!(!agent_may_delete(&direct, "beta"));

        let mut grabbed = owner_shared("grabbed");
        grabbed.grabbed_by = Some("alpha".to_string());
        assert!(agent_may_delete(&grabbed, "alpha"));
        assert!(!agent_may_delete(&grabbed, "beta"));

        let mut sent = owner_shared("sent");
        sent.direction = StoredDirection::AgentToOwner;
        sent.from = "alpha".to_string();
        assert!(agent_may_delete(&sent, "alpha"));
        assert!(!agent_may_delete(&sent, "beta"));
    }

    /// Deleted-id label used by `apply_delete` on success.
    fn deleted_id(outcome: &DeleteOutcome) -> Option<&str> {
        match outcome {
            DeleteOutcome::Deleted(id) => Some(id.as_str()),
            _ => None,
        }
    }

    #[test]
    fn apply_delete_owner_removes_any_message() {
        let mut items = vec![owner_shared("a"), owner_shared("b")];
        let (outcome, changed) = apply_delete(&mut items, "a", None);
        assert_eq!(deleted_id(&outcome), Some("a"));
        assert!(changed);
        assert_eq!(
            items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
            vec!["b"]
        );
    }

    #[test]
    fn apply_delete_agent_only_its_own() {
        let mut direct = owner_shared("direct");
        direct.to = Some("alpha".to_string());
        let mut items = vec![owner_shared("open"), direct];

        // A shared item the agent never touched: forbidden, list unchanged.
        let (outcome, changed) = apply_delete(&mut items, "open", Some("alpha"));
        assert!(matches!(outcome, DeleteOutcome::Forbidden));
        assert!(!changed);
        assert_eq!(items.len(), 2);

        // A message addressed to the agent: removed.
        let (outcome, changed) = apply_delete(&mut items, "direct", Some("alpha"));
        assert_eq!(deleted_id(&outcome), Some("direct"));
        assert!(changed);
        assert_eq!(
            items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
            vec!["open"]
        );
    }

    #[test]
    fn agent_can_see_matches_view_membership() {
        // Shared open queue: visible to any agent.
        assert!(agent_can_see(&owner_shared("open"), "alpha"));

        // A direct message is visible only to its addressee — the audience wall
        // that keeps a secret sent to one agent from another.
        let mut direct = owner_shared("direct");
        direct.to = Some("alpha".to_string());
        assert!(agent_can_see(&direct, "alpha"));
        assert!(!agent_can_see(&direct, "beta"));

        let mut grabbed = owner_shared("grabbed");
        grabbed.grabbed_by = Some("alpha".to_string());
        assert!(agent_can_see(&grabbed, "alpha"));
        assert!(!agent_can_see(&grabbed, "beta"));

        let mut sent = owner_shared("sent");
        sent.direction = StoredDirection::AgentToOwner;
        sent.from = "alpha".to_string();
        assert!(agent_can_see(&sent, "alpha"));
        assert!(!agent_can_see(&sent, "beta"));
    }

    #[test]
    fn apply_delete_missing_id_is_not_found_and_no_change() {
        let mut items = vec![owner_shared("a")];
        let (outcome, changed) = apply_delete(&mut items, "nope", None);
        assert!(matches!(outcome, DeleteOutcome::NotFound));
        assert!(!changed);
        assert_eq!(items.len(), 1);
    }

    #[test]
    fn validate_text_rejects_empty_and_oversized() {
        assert!(validate_text("id", "", false).is_some());
        // Empty is allowed when a file is attached (a caption-less file).
        assert!(validate_text("id", "", true).is_none());
        assert!(validate_text("id", "hello", false).is_none());
        let big = "x".repeat(MAX_TEXT_BYTES + 1);
        assert!(validate_text("id", &big, false).is_some());
        // Even with a file, an over-long caption is rejected.
        assert!(validate_text("id", &big, true).is_some());
        let ok = "x".repeat(MAX_TEXT_BYTES);
        assert!(validate_text("id", &ok, false).is_none());
    }

    #[test]
    fn validate_label_caps_length() {
        assert!(validate_label("id", "from", None).is_none());
        assert!(validate_label("id", "from", Some("alpha")).is_none());
        let big = "x".repeat(MAX_LABEL_BYTES + 1);
        assert!(validate_label("id", "grabbed_by", Some(&big)).is_some());
    }

    #[test]
    fn validate_mime_caps_length() {
        assert!(validate_mime("id", "image/png").is_none());
        assert!(validate_mime("id", &"x".repeat(MAX_MIME_BYTES)).is_none());
        assert!(validate_mime("id", &"x".repeat(MAX_MIME_BYTES + 1)).is_some());
    }

    #[test]
    fn gram_item_to_info_carries_origin_id_to_the_wire() {
        let mut item = owner_shared("m1");
        item.origin_id = "machine_abc123".to_string();
        assert_eq!(
            gram_item_to_info(item, &MachineLabels::new()).origin_id,
            "machine_abc123"
        );
    }

    #[test]
    fn wire_message_without_origin_id_decodes_to_empty() {
        // A message serialized by an older daemon has no origin_id; it must still
        // decode (empty), so the app keeps rendering old grams. See issue #98.
        let legacy = serde_json::json!({
            "id": "gram-1-2-3",
            "direction": "agent_to_owner",
            "from": "alpha",
            "text": "hi",
            "created_unix_ms": 1u64,
        });
        let info: GramMessageInfo = serde_json::from_value(legacy).unwrap();
        assert_eq!(info.origin_id, "");
    }

    #[test]
    fn gram_send_stamps_and_returns_the_stable_store_origin_id() {
        // Redirect config-home to a throwaway dir so the send writes to a temp
        // store (never the real ~/.config/herdr/gram.json) and machine::get_or_create
        // mints a temp id. nextest runs each test in its own process, so the
        // machine-id OnceLock and this env var stay isolated to this test.
        let _guard = crate::config::test_config_env_lock().lock().unwrap();
        let prev_xdg = std::env::var_os("XDG_CONFIG_HOME");
        let tmp = std::env::temp_dir().join(format!(
            "herdr-gram-origin-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        std::env::set_var("XDG_CONFIG_HOME", &tmp);

        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );

        let send = |app: &mut App, req: &str| -> serde_json::Value {
            let raw = app.handle_gram_send(
                req.to_string(),
                GramSendParams {
                    text: "ping".to_string(),
                    caller_pane_id: None,
                    from: Some("tester".to_string()),
                    file: None,
                },
            );
            serde_json::from_str(&raw).unwrap()
        };

        let first = send(&mut app, "req-1");
        let store_id = first["result"]["store_id"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let origin_id = first["result"]["message"]["origin_id"]
            .as_str()
            .unwrap_or("");
        // The mint site actually stamped the stable install id (not an empty string
        // or the volatile pid), and it is echoed on the send response envelope.
        assert!(store_id.starts_with("machine_"), "store_id: {store_id:?}");
        assert_eq!(
            origin_id, store_id,
            "message.origin_id must equal the store it landed in"
        );

        // A second send carries the SAME origin_id — the stability the pid lacked
        // (a restart would have changed the pid segment; the store id does not).
        let second = send(&mut app, "req-2");
        assert_eq!(
            second["result"]["message"]["origin_id"]
                .as_str()
                .unwrap_or(""),
            store_id
        );

        match prev_xdg {
            Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn owner_discards_a_staged_upload_but_never_a_posted_one() {
        let _guard = crate::config::test_config_env_lock().lock().unwrap();
        let prev_xdg = std::env::var_os("XDG_CONFIG_HOME");
        let tmp = std::env::temp_dir().join(format!(
            "herdr-gram-discard-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        std::env::set_var("XDG_CONFIG_HOME", &tmp);
        let staging = crate::config::config_dir()
            .join("gram-files")
            .join(".staging");

        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let upload = |app: &mut App, upload_id: &str| {
            let raw = app.handle_gram_upload_chunk(
                "u".into(),
                serde_json::from_value(serde_json::json!(
                    {"upload_id": upload_id, "offset": 0, "data_base64": "aGVsbG8="}
                ))
                .unwrap(),
            );
            let reply: serde_json::Value = serde_json::from_str(&raw).unwrap();
            assert_eq!(reply["result"]["type"], "ok", "{reply}");
        };
        let discard = |app: &mut App, upload_id: &str| -> serde_json::Value {
            let raw = app.handle_gram_upload_discard(
                "d".into(),
                GramUploadDiscardParams {
                    upload_id: upload_id.into(),
                },
            );
            serde_json::from_str(&raw).unwrap()
        };
        let post = |app: &mut App, upload_id: &str| -> serde_json::Value {
            let raw = app.handle_gram_post(
                "p".into(),
                serde_json::from_value(serde_json::json!(
                    {"text": "notes", "file": {"upload_id": upload_id, "name": "a.txt", "mime": "text/plain"}}
                ))
                .unwrap(),
            );
            serde_json::from_str(&raw).unwrap()
        };

        // A staged upload is deleted, and can no longer be attached.
        upload(&mut app, "up-removed");
        assert!(staging.join("up-removed").exists());
        let discarded = discard(&mut app, "up-removed");
        assert_eq!(discarded["result"]["type"], "gram_upload_discarded");
        assert_eq!(discarded["result"]["discarded"], true, "{discarded}");
        assert!(!staging.join("up-removed").exists());
        let refused = post(&mut app, "up-removed");
        assert_eq!(refused["error"]["code"], "invalid_params", "{refused}");
        assert!(
            refused["error"]["message"]
                .as_str()
                .unwrap()
                .contains("no staged upload"),
            "{refused}"
        );

        // Unknown and already-discarded ids are a normal `false`, not an error.
        assert_eq!(
            discard(&mut app, "up-removed")["result"]["discarded"],
            false
        );
        assert_eq!(
            discard(&mut app, "never-uploaded")["result"]["discarded"],
            false
        );

        // A posted upload was consumed: discarding it is `false` and the
        // message keeps its file.
        upload(&mut app, "up-posted");
        let posted = post(&mut app, "up-posted");
        let message_id = posted["result"]["message"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(discard(&mut app, "up-posted")["result"]["discarded"], false);
        let file: serde_json::Value = serde_json::from_str(&app.handle_gram_get_file(
            "g".into(),
            GramGetFileParams {
                id: message_id,
                caller_pane_id: None,
            },
        ))
        .unwrap();
        assert_eq!(file["result"]["data_base64"], "aGVsbG8=", "{file}");

        // A live writer on the id keeps its bytes.
        upload(&mut app, "up-streaming");
        let claim = crate::api::UploadClaim::acquire("up-streaming").unwrap();
        let busy = discard(&mut app, "up-streaming");
        assert_eq!(busy["error"]["code"], "upload_in_progress", "{busy}");
        assert!(staging.join("up-streaming").exists());
        drop(claim);
        assert_eq!(
            discard(&mut app, "up-streaming")["result"]["discarded"],
            true
        );

        match prev_xdg {
            Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The digest must be a function of the ANSWER, so an unchanged store yields the
    /// same fingerprint on every call. Without this the conditional poll never matches
    /// and the whole store ships every 6 seconds, which is the bug being fixed.
    #[test]
    fn list_digest_is_stable_for_the_same_answer() {
        let messages = vec![gram_item_to_info(owner_shared("g1"), &MachineLabels::new())];
        assert_eq!(
            list_digest("store-1", &messages),
            list_digest("store-1", &messages)
        );
    }

    /// ...and a function of the CONTENT, so any change the client would render also
    /// changes the digest. Hashing the serialized payload is what buys this for every
    /// field at once, including ones added later.
    #[test]
    fn list_digest_changes_with_content_and_with_the_store() {
        let base = vec![gram_item_to_info(owner_shared("g1"), &MachineLabels::new())];
        let mut read = owner_shared("g1");
        read.read_by_owner = false;
        let flipped = vec![gram_item_to_info(read, &MachineLabels::new())];
        let two = vec![
            gram_item_to_info(owner_shared("g1"), &MachineLabels::new()),
            gram_item_to_info(owner_shared("g2"), &MachineLabels::new()),
        ];

        let digest = list_digest("store-1", &base);
        assert_ne!(
            digest,
            list_digest("store-1", &flipped),
            "a read flag change"
        );
        assert_ne!(digest, list_digest("store-1", &two), "a new message");
        assert_ne!(digest, list_digest("store-2", &base), "a different store");
    }

    /// End to end through the real dispatch: a matching digest answers
    /// `gram_list_unchanged` and carries NO messages, and a stale one answers in full.
    /// The "no messages" half is the point — an answer that still shipped the list
    /// would save nothing.
    #[test]
    fn conditional_list_answers_unchanged_only_while_the_digest_matches() {
        let _guard = crate::config::test_config_env_lock().lock().unwrap();
        let prev_xdg = std::env::var_os("XDG_CONFIG_HOME");
        let tmp = std::env::temp_dir().join(format!(
            "herdr-gram-digest-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        std::env::set_var("XDG_CONFIG_HOME", &tmp);

        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let list = |app: &mut App, digest: Option<&str>| -> serde_json::Value {
            let raw = app.handle_gram_list(
                "req".to_string(),
                GramListParams {
                    caller_pane_id: None,
                    only_queue: false,
                    unread_only: false,
                    if_unchanged_digest: digest.map(str::to_string),
                    limit: None,
                    before_id: None,
                },
            );
            serde_json::from_str(&raw).unwrap()
        };

        // Seed one message so the answer is not trivially empty.
        app.handle_gram_send(
            "seed".to_string(),
            GramSendParams {
                text: "ping".to_string(),
                caller_pane_id: None,
                from: Some("tester".to_string()),
                file: None,
            },
        );

        let full = list(&mut app, None);
        assert_eq!(full["result"]["type"], "gram_list");
        assert_eq!(full["result"]["messages"].as_array().unwrap().len(), 1);
        let digest = full["result"]["digest"].as_str().unwrap().to_string();

        // Omitting the parameter must ALWAYS produce a full list: an old client that
        // knows nothing about digests can never be answered "unchanged".
        assert_eq!(list(&mut app, None)["result"]["type"], "gram_list");

        let unchanged = list(&mut app, Some(&digest));
        assert_eq!(unchanged["result"]["type"], "gram_list_unchanged");
        assert!(
            unchanged["result"]["messages"].is_null(),
            "an unchanged answer must not carry the list it just saved sending"
        );
        assert_eq!(unchanged["result"]["digest"], digest.as_str());

        // A stale digest is answered in full.
        let stale = list(&mut app, Some("not-the-digest"));
        assert_eq!(stale["result"]["type"], "gram_list");
        assert_eq!(stale["result"]["messages"].as_array().unwrap().len(), 1);

        // And a real change invalidates the digest the client holds.
        app.handle_gram_send(
            "seed-2".to_string(),
            GramSendParams {
                text: "second".to_string(),
                caller_pane_id: None,
                from: Some("tester".to_string()),
                file: None,
            },
        );
        let after = list(&mut app, Some(&digest));
        assert_eq!(
            after["result"]["type"], "gram_list",
            "a store that moved must not be reported unchanged"
        );
        assert_eq!(after["result"]["messages"].as_array().unwrap().len(), 2);

        match prev_xdg {
            Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Seed a private gram store, then read it through the real handler. Paging is
    /// only observable end to end — the cut depends on the audience filter and the
    /// newest-first reverse — so these tests go through `handle_gram_list` and its
    /// encoded answer rather than a helper in isolation.
    fn with_gram_store<T>(items: &[GramItem], body: impl FnOnce(&mut App) -> T) -> T {
        let _guard = crate::config::test_config_env_lock().lock();
        let prev_xdg = std::env::var_os("XDG_CONFIG_HOME");
        let tmp = std::env::temp_dir().join(format!(
            "herdr-gram-page-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        std::env::set_var("XDG_CONFIG_HOME", &tmp);

        for item in items {
            crate::persist::gram::append(item.clone()).unwrap();
        }
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let outcome = body(&mut app);

        match prev_xdg {
            Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        let _ = std::fs::remove_dir_all(&tmp);
        outcome
    }

    fn list(app: &mut App, params: GramListParams) -> serde_json::Value {
        serde_json::from_str(&app.handle_gram_list("req".to_string(), params)).unwrap()
    }

    fn page(app: &mut App, limit: Option<usize>, before_id: Option<&str>) -> serde_json::Value {
        list(
            app,
            GramListParams {
                limit,
                before_id: before_id.map(str::to_string),
                ..Default::default()
            },
        )
    }

    fn page_ids(answer: &serde_json::Value) -> Vec<String> {
        answer["result"]["messages"]
            .as_array()
            .expect("a gram_list answer carries messages")
            .iter()
            .map(|message| message["id"].as_str().unwrap().to_string())
            .collect()
    }

    /// Oldest-first, the order the store keeps.
    fn seeded(count: usize) -> Vec<GramItem> {
        (0..count)
            .map(|index| {
                let mut item = owner_shared(&format!("g{index}"));
                item.created_unix_ms = index as u64 + 1;
                item
            })
            .collect()
    }

    /// The compatibility floor: an old client that sends no paging parameters must
    /// get exactly what it got before they existed — every message, newest first —
    /// and `has_more` false, since an unpaged answer already reaches the oldest.
    #[test]
    fn list_without_paging_params_returns_the_whole_list_newest_first() {
        let items = seeded(4);
        let answer = with_gram_store(&items, |app| page(app, None, None));
        assert_eq!(answer["result"]["type"], "gram_list");
        assert_eq!(page_ids(&answer), vec!["g3", "g2", "g1", "g0"]);
        assert_eq!(answer["result"]["has_more"], false);
    }

    #[test]
    fn limit_cuts_the_newest_page_and_reports_more() {
        let items = seeded(5);
        let answer = with_gram_store(&items, |app| page(app, Some(2), None));
        assert_eq!(page_ids(&answer), vec!["g4", "g3"]);
        assert_eq!(answer["result"]["has_more"], true);
    }

    /// A limit past the end is not an edge case for the client to special-case: it
    /// gets the whole list and is told there is nothing older.
    #[test]
    fn limit_larger_than_the_list_returns_everything_with_no_more() {
        let items = seeded(3);
        let answer = with_gram_store(&items, |app| page(app, Some(50), None));
        assert_eq!(page_ids(&answer), vec!["g2", "g1", "g0"]);
        assert_eq!(answer["result"]["has_more"], false);
    }

    /// Walking the cursor is the actual scroll the app performs: every page strictly
    /// older than the last id it holds, no id twice, and the walk terminates covering
    /// the list exactly once. A cursor that were inclusive, or an off-by-one on the
    /// reverse, shows up here as a duplicate or a hole.
    #[test]
    fn walking_before_id_covers_the_list_exactly_once() {
        let items = seeded(7);
        let walked = with_gram_store(&items, |app| {
            let mut seen: Vec<String> = Vec::new();
            let mut cursor: Option<String> = None;
            loop {
                let answer = page(app, Some(3), cursor.as_deref());
                let ids = page_ids(&answer);
                assert!(!ids.is_empty(), "a page with has_more must not be empty");
                for id in &ids {
                    assert!(!seen.contains(id), "page repeated {id}");
                }
                seen.extend(ids.iter().cloned());
                if answer["result"]["has_more"] == false {
                    break;
                }
                cursor = ids.last().cloned();
            }
            seen
        });
        assert_eq!(walked, vec!["g6", "g5", "g4", "g3", "g2", "g1", "g0"]);
    }

    /// A cursor that is not in the list is an error, never a silent page 1: falling
    /// back to the head would re-deliver the newest page on every scroll, so the
    /// reader could never reach older messages.
    #[test]
    fn unknown_before_id_is_rejected() {
        let items = seeded(3);
        let answer = with_gram_store(&items, |app| page(app, Some(2), Some("g-nope")));
        assert_eq!(answer["error"]["code"], "invalid_params");
        assert_eq!(
            answer["error"]["message"], "before_id is not in this list",
            "the client needs to know its cursor aged out, not get page 1 back"
        );
    }

    /// A `before_id` that exists but was filtered OUT of this audience is just as
    /// unusable as one that never existed — it must not silently anchor at the head.
    #[test]
    fn before_id_outside_the_filtered_list_is_rejected() {
        let mut items = seeded(3);
        let mut unread = owner_shared("only-unread");
        unread.direction = StoredDirection::AgentToOwner;
        unread.read_by_owner = false;
        items.push(unread);

        let answer = with_gram_store(&items, |app| {
            list(
                app,
                GramListParams {
                    unread_only: true,
                    before_id: Some("g1".to_string()),
                    ..Default::default()
                },
            )
        });
        assert_eq!(answer["error"]["code"], "invalid_params");
    }

    /// Zero is rejected rather than answered with an empty page: a reader cannot tell
    /// an empty page from the end of the list, so it would simply stop scrolling.
    #[test]
    fn zero_limit_is_rejected() {
        let items = seeded(2);
        let answer = with_gram_store(&items, |app| page(app, Some(0), None));
        assert_eq!(answer["error"]["code"], "invalid_params");
        assert!(answer["result"].is_null());
    }

    /// ...but an over-large limit is CLAMPED, not rejected: an over-eager client
    /// still gets a valid bounded page it can page onward from.
    #[test]
    fn limit_above_the_cap_is_clamped_not_rejected() {
        let items = seeded(GRAM_LIST_MAX_LIMIT + 3);
        let answer = with_gram_store(&items, |app| page(app, Some(GRAM_LIST_MAX_LIMIT * 4), None));
        assert_eq!(answer["result"]["type"], "gram_list");
        assert_eq!(
            answer["result"]["messages"].as_array().unwrap().len(),
            GRAM_LIST_MAX_LIMIT
        );
        assert_eq!(
            answer["result"]["has_more"], true,
            "a clamped page must still admit that older messages remain"
        );
    }

    /// The badge and Read-all read the inbox, not the window: unread messages that
    /// fall entirely outside page 1 must still be counted. Here every unread message
    /// is older than the page, so a count taken over the page would report zero.
    #[test]
    fn unread_count_covers_the_whole_filtered_list_not_the_page() {
        let mut items: Vec<GramItem> = (0..3)
            .map(|index| {
                let mut unread = owner_shared(&format!("old-unread-{index}"));
                unread.direction = StoredDirection::AgentToOwner;
                unread.read_by_owner = false;
                unread.created_unix_ms = index as u64 + 1;
                unread
            })
            .collect();
        items.extend((0..4).map(|index| {
            let mut item = owner_shared(&format!("new-read-{index}"));
            item.created_unix_ms = index as u64 + 10;
            item
        }));

        let answer = with_gram_store(&items, |app| page(app, Some(2), None));
        assert_eq!(page_ids(&answer), vec!["new-read-3", "new-read-2"]);
        assert_eq!(answer["result"]["unread_count"], 3);
    }

    /// Conditional fetch is HEAD-only. With a cursor the client is asking for a page
    /// it does not hold, so a matching digest must not short-circuit it; without one,
    /// the unchanged answer still works. The digest itself stays a fingerprint of the
    /// full list, so the same value keeps working for the cheap head poll.
    #[test]
    fn digest_short_circuits_the_head_but_never_an_older_page() {
        let items = seeded(5);
        with_gram_store(&items, |app| {
            let head = page(app, Some(2), None);
            let digest = head["result"]["digest"].as_str().unwrap().to_string();

            let unchanged = list(
                app,
                GramListParams {
                    limit: Some(2),
                    if_unchanged_digest: Some(digest.clone()),
                    ..Default::default()
                },
            );
            assert_eq!(unchanged["result"]["type"], "gram_list_unchanged");

            let older = list(
                app,
                GramListParams {
                    limit: Some(2),
                    before_id: Some("g3".to_string()),
                    if_unchanged_digest: Some(digest.clone()),
                    ..Default::default()
                },
            );
            assert_eq!(
                older["result"]["type"], "gram_list",
                "an older page must be answered even while the head is unchanged"
            );
            assert_eq!(page_ids(&older), vec!["g2", "g1"]);

            // The digest a paging client polls with is the FULL list's, so it does not
            // move when the page size does.
            let wider = page(app, Some(4), None);
            assert_eq!(wider["result"]["digest"].as_str().unwrap(), digest);
        });
    }

    /// The agent view pages on the same terms, over its own audience: the cursor
    /// indexes the filtered list, so an item the agent cannot see is neither a page
    /// entry nor a usable anchor.
    #[test]
    fn agent_view_pages_over_its_own_audience() {
        let mut items = seeded(4);
        let mut other = owner_shared("addressed-elsewhere");
        other.to = Some("someone-else".to_string());
        other.created_unix_ms = 99;
        items.push(other);

        let (first, second) = with_gram_store(&items, |app| {
            app.state.workspaces = vec![crate::workspace::Workspace::test_new("gram-paging")];
            app.state.ensure_test_terminals();
            let pane_id = app.state.workspaces[0].tabs[0].root_pane;
            let caller = app.public_pane_id(0, pane_id).unwrap();

            let first = list(
                app,
                GramListParams {
                    caller_pane_id: Some(caller.clone()),
                    limit: Some(2),
                    ..Default::default()
                },
            );
            let cursor = page_ids(&first).last().unwrap().clone();
            let second = list(
                app,
                GramListParams {
                    caller_pane_id: Some(caller),
                    limit: Some(2),
                    before_id: Some(cursor),
                    ..Default::default()
                },
            );
            (first, second)
        });

        // The direct message to another agent is invisible here, so it is not the
        // newest entry the way it would be in the owner view.
        assert_eq!(page_ids(&first), vec!["g3", "g2"]);
        assert_eq!(first["result"]["has_more"], true);
        assert_eq!(page_ids(&second), vec!["g1", "g0"]);
        assert_eq!(second["result"]["has_more"], false);
    }

    /// Paging did not open a back door for the owner-only filter: `unread_only` with a
    /// caller pane is still rejected rather than quietly ignored.
    #[test]
    fn unread_only_with_a_caller_pane_is_still_rejected() {
        let items = seeded(2);
        let answer = with_gram_store(&items, |app| {
            app.state.workspaces = vec![crate::workspace::Workspace::test_new("gram-unread")];
            app.state.ensure_test_terminals();
            let pane_id = app.state.workspaces[0].tabs[0].root_pane;
            let caller = app.public_pane_id(0, pane_id).unwrap();
            list(
                app,
                GramListParams {
                    caller_pane_id: Some(caller),
                    unread_only: true,
                    limit: Some(1),
                    ..Default::default()
                },
            )
        });
        assert_eq!(answer["error"]["code"], "invalid_params");
    }

    /// A federated machine's agent as the coordinator caches it, stamped with
    /// the machine's label.
    fn labeled_peer_agent(alias: &str, label: &str) -> crate::api::schema::AgentInfo {
        serde_json::from_value(serde_json::json!({
            "terminal_id": format!("{alias}/t1"),
            "name": format!("{alias}/llm-opt"),
            "agent": "omp",
            "agent_status": "idle",
            "workspace_id": format!("{alias}/w1"),
            "tab_id": format!("{alias}/w1:t1"),
            "pane_id": format!("{alias}/w1-1"),
            "focused": false,
            "revision": 1,
            "machine_id": alias,
            "machine_label": label,
            "reachability": "reachable",
        }))
        .unwrap()
    }

    #[test]
    fn owner_list_labels_only_grams_relayed_from_a_labeled_machine() {
        const STUDIO: &str = "8195b6326f748f4da1945364a4e205b9";
        let sent = |id: &str, from: &str| GramItem {
            direction: StoredDirection::AgentToOwner,
            from: from.to_string(),
            to: None,
            read_by_owner: false,
            ..owner_shared(id)
        };
        let mut post = owner_shared("post");
        post.to = Some(format!("{STUDIO}/llm-opt"));
        let items = [
            sent("relayed", &format!("{STUDIO}/llm-opt")),
            sent("unknown", "0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f/llm-opt"),
            // An explicit peer without a display label is labeled by its alias.
            sent("bare", "home/builder"),
            sent("local", "llm-local"),
            post,
        ];
        let answer = with_gram_store(&items, |app| {
            {
                let mut store = app.federation.lock().unwrap();
                for (alias, label) in [(STUDIO, "Jerry's Mac Studio"), ("home", "home")] {
                    store.set_peer(
                        alias,
                        crate::api::federation_store::PeerCacheEntry::reachable(
                            vec![labeled_peer_agent(alias, label)],
                            std::time::Instant::now(),
                        ),
                    );
                }
            }
            list(app, GramListParams::default())
        });
        let messages = answer["result"]["messages"].as_array().unwrap();
        let mut labels: Vec<_> = messages
            .iter()
            .map(|message| {
                (
                    message["id"].as_str().unwrap(),
                    message["from"].as_str().unwrap(),
                    message
                        .get("machine_label")
                        .and_then(|label| label.as_str()),
                )
            })
            .collect();
        labels.sort_unstable();
        assert_eq!(
            labels,
            [
                ("bare", "home/builder", None),
                ("local", "llm-local", None),
                ("post", "owner", None),
                (
                    "relayed",
                    "8195b6326f748f4da1945364a4e205b9/llm-opt",
                    Some("Jerry's Mac Studio")
                ),
                ("unknown", "0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f/llm-opt", None),
            ],
            "{answer}"
        );
    }

    #[cfg(unix)]
    mod relay_reload {
        use super::*;

        const PEER: &str = "8195b6326f748f4da1945364a4e205b9";
        const COORDINATOR: &str = "machine_f872cca4c7743d39c1fad1fa5a74a087";

        fn app_with_config_file() -> (App, std::path::PathBuf) {
            let tmp = std::env::temp_dir().join(format!(
                "herdr-gram-relay-reload-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&tmp).unwrap();
            std::env::set_var("XDG_CONFIG_HOME", &tmp);
            std::env::remove_var(crate::api::gram_relay::PEERS_ENV);
            std::env::remove_var(crate::api::gram_relay::SOCKET_ENV);
            let path = tmp.join("config.toml");
            std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &path);
            let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
            let app = App::new(
                &crate::config::Config::default(),
                crate::app::AppPolicy::TEST,
                None,
                api_rx,
                crate::api::EventHub::default(),
            );
            (app, path)
        }

        fn reload(
            app: &mut App,
            path: &std::path::Path,
            toml: &str,
        ) -> crate::config::ConfigReloadStatus {
            std::fs::write(path, toml).unwrap();
            app.apply_config_from_disk(false).status
        }

        fn relay_list(app: &mut App) -> serde_json::Value {
            serde_json::from_str(&app.handle_gram_relay(
                "relay".into(),
                GramRelayParams {
                    peer_alias: PEER.into(),
                    call: GramRelayCall::List(GramListParams {
                        caller_pane_id: None,
                        only_queue: false,
                        unread_only: false,
                        if_unchanged_digest: None,
                        limit: None,
                        before_id: None,
                    }),
                },
            ))
            .unwrap()
        }

        fn status(app: &App) -> serde_json::Value {
            serde_json::from_str::<serde_json::Value>(&app.handle_gram_relay_status("s".into()))
                .unwrap()["result"]
                .clone()
        }

        #[test]
        fn reload_grants_then_revokes_a_peer_without_restart() {
            let (mut app, path) = app_with_config_file();
            assert_eq!(relay_list(&mut app)["error"]["code"], "forbidden");

            reload(
                &mut app,
                &path,
                &format!("[gram_relay]\npeers = [\"{PEER}\"]\n"),
            );
            let granted = relay_list(&mut app);
            assert_ne!(granted["error"]["code"], "forbidden", "{granted}");
            assert_eq!(status(&app)["coordinator"]["source"], "config");
            assert_eq!(
                status(&app)["coordinator"]["effective"],
                serde_json::json!([PEER])
            );

            reload(&mut app, &path, "");
            assert_eq!(relay_list(&mut app)["error"]["code"], "forbidden");
            assert_eq!(
                status(&app)["coordinator"]["effective"],
                serde_json::Value::Null
            );
        }

        #[test]
        fn explicit_empty_peers_revoke_a_peer_the_legacy_env_still_names() {
            let (mut app, path) = app_with_config_file();
            std::env::set_var(crate::api::gram_relay::PEERS_ENV, PEER);
            reload(
                &mut app,
                &path,
                &format!("[gram_relay]\npeers = [\"{PEER}\"]\n"),
            );
            assert_ne!(relay_list(&mut app)["error"]["code"], "forbidden");
            assert_eq!(status(&app)["coordinator"]["source"], "config");

            reload(&mut app, &path, "[gram_relay]\npeers = []\n");
            assert_eq!(relay_list(&mut app)["error"]["code"], "forbidden");
            let coordinator = status(&app)["coordinator"].clone();
            assert_eq!(coordinator["configured"], serde_json::json!([]));
            assert_eq!(coordinator["environment"], "present");
            assert_eq!(coordinator["effective"], serde_json::Value::Null);
            assert_eq!(coordinator["error"], "conflict");
            std::env::remove_var(crate::api::gram_relay::PEERS_ENV);
        }

        #[test]
        fn invalid_reload_keeps_the_previous_effective_setting() {
            let (mut app, path) = app_with_config_file();
            reload(
                &mut app,
                &path,
                &format!("[gram_relay]\npeers = [\"{PEER}\"]\ncoordinator_machine_id = \"{COORDINATOR}\"\n"),
            );
            let before = status(&app)["remote"]["effective_socket"].clone();
            assert!(before.is_string());

            let outcome = reload(
                &mut app,
                &path,
                "[gram_relay]\npeers = []\ncoordinator_machine_id = \"not-a-machine\"\n",
            );
            assert_eq!(outcome, crate::config::ConfigReloadStatus::Partial);
            assert_ne!(relay_list(&mut app)["error"]["code"], "forbidden");
            let after = status(&app);
            assert_eq!(after["remote"]["effective_socket"], before);
            assert_eq!(after["remote"]["error"], "invalid_config");
            assert_eq!(after["coordinator"]["error"], "invalid_config");
            assert!(after["remote"]["message"]
                .as_str()
                .unwrap()
                .contains("coordinator_machine_id"));
        }

        #[test]
        fn remote_socket_follows_reload() {
            let (mut app, path) = app_with_config_file();
            let own = crate::persist::machine::get_or_create();
            reload(
                &mut app,
                &path,
                &format!("[gram_relay]\ncoordinator_machine_id = \"{COORDINATOR}\"\n"),
            );
            let expected = crate::api::gram_relay::reverse_socket_path(COORDINATOR, &own);
            assert_eq!(
                crate::api::gram_relay::policy().remote_socket(),
                Some(expected.clone())
            );
            let remote = status(&app)["remote"].clone();
            assert_eq!(remote["effective_socket"], expected.display().to_string());
            assert_eq!(remote["environment"], "absent");
            assert_eq!(remote["accepting"], false);

            reload(&mut app, &path, "");
            assert_eq!(crate::api::gram_relay::policy().remote_socket(), None);
            assert_eq!(status(&app)["remote"]["accepting"], serde_json::Value::Null);
        }

        /// A coordinator allowing `PEER`, whose roster has `llm-opt` in pane
        /// `w1-1` and `other-agent` in `w1-2`, merged as the coordinator's
        /// federation poll merges them: every name and id `PEER/`-qualified.
        fn coordinator() -> App {
            let (mut app, path) = app_with_config_file();
            reload(
                &mut app,
                &path,
                &format!("[gram_relay]\npeers = [\"{PEER}\"]\n"),
            );
            let presentation = crate::api::federation_manager::PeerPresentation {
                profile_id: Some(PEER.into()),
                label: "Jerrys-Mac-Studio".into(),
            };
            let agent = |pane: &str, name: &str| -> crate::api::schema::AgentInfo {
                let local = serde_json::from_value(serde_json::json!({
                    "terminal_id": format!("term_65c2c15717c84.{pane}"),
                    "name": name,
                    "agent": "omp",
                    "agent_status": "idle",
                    "workspace_id": "w1",
                    "tab_id": "w1:t1",
                    "pane_id": pane,
                    "focused": false,
                    "revision": 1,
                }))
                .unwrap();
                crate::api::prefix_remote_agent(PEER, &presentation, local).unwrap()
            };
            app.federation.lock().unwrap().set_peer(
                PEER,
                crate::api::federation_store::PeerCacheEntry::reachable(
                    vec![agent("w1-1", "llm-opt"), agent("w1-2", "other-agent")],
                    std::time::Instant::now(),
                ),
            );
            app
        }

        fn relay(app: &mut App, call: serde_json::Value) -> serde_json::Value {
            let call: GramRelayCall = serde_json::from_value(call).unwrap();
            serde_json::from_str(&app.handle_gram_relay(
                "relay".into(),
                GramRelayParams {
                    peer_alias: PEER.into(),
                    call,
                },
            ))
            .unwrap()
        }

        fn guest_post(to: &str, guest: &str) -> serde_json::Value {
            serde_json::json!({"kind": "post", "params": {"text": "notes", "to": to, "guest": guest}})
        }

        #[test]
        fn a_relayed_guest_post_reaches_the_remote_agent_with_its_file() {
            let mut app = coordinator();
            let staged = relay(
                &mut app,
                serde_json::json!({"kind": "upload_chunk", "params": {"upload_id": "guest-g-up", "offset": 0, "data_base64": "aGVsbG8="}}),
            );
            assert_eq!(staged["result"]["type"], "ok", "{staged}");
            let posted = relay(
                &mut app,
                serde_json::json!({"kind": "post", "params": {"text": "notes", "to": "llm-opt", "guest": "plotarmordev", "file": {"upload_id": "guest-g-up", "name": "a.txt", "mime": "text/plain"}}}),
            );
            let message = &posted["result"]["message"];
            assert_eq!(message["direction"], "owner_to_agent", "{posted}");
            // Addressed as the coordinator names that machine's agent.
            assert_eq!(message["to"], format!("{PEER}/llm-opt"));
            let id = message["id"].as_str().unwrap();

            let listed = relay(
                &mut app,
                serde_json::json!({"kind": "list", "params": {"caller_pane_id": "w1-1"}}),
            );
            let found = listed["result"]["messages"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["id"] == id)
                .cloned()
                .unwrap_or_else(|| panic!("the agent lists the guest's post: {listed}"));
            assert_eq!(found["from"], "plotarmordev (via HerdrUp)");
            let chunk = relay(
                &mut app,
                serde_json::json!({"kind": "get_file_chunk", "params": {"id": id, "offset": 0, "caller_pane_id": "w1-1"}}),
            );
            assert_eq!(chunk["result"]["data_base64"], "aGVsbG8=", "{chunk}");

            let others = relay(
                &mut app,
                serde_json::json!({"kind": "list", "params": {"caller_pane_id": "w1-2"}}),
            );
            assert!(
                !others.to_string().contains(id),
                "another agent does not see it: {others}"
            );
        }

        #[test]
        fn relayed_posts_are_only_guest_posts_to_the_peers_own_agents() {
            let mut app = coordinator();
            for call in [
                guest_post("llm-opt", ""),
                guest_post("llm-opt", "owner (via HerdrUp)"),
                guest_post("not-on-the-peer", "plotarmordev"),
                guest_post("", "plotarmordev"),
                // The remote names its own agent; a qualified name is not one.
                guest_post(&format!("{PEER}/llm-opt"), "plotarmordev"),
            ] {
                let answer = relay(&mut app, call.clone());
                assert_eq!(answer["error"]["code"], "forbidden", "{call} -> {answer}");
            }
            let answer = relay(&mut app, guest_post("other-agent", "friend"));
            assert_eq!(answer["result"]["type"], "gram_sent", "{answer}");
        }

        #[test]
        fn a_relayed_gram_carries_its_machine_label_to_lists_and_the_owner_push() {
            let mut app = coordinator();
            app.state.push_config = crate::config::PushConfig {
                mode: crate::config::PushMode::Direct,
                enabled: true,
                key_path: Some("/tmp/AuthKey.p8".to_string()),
                key_id: Some("ABC123DEFG".to_string()),
                team_id: Some("TEAM123456".to_string()),
                topic: Some("app.herdr.ios".to_string()),
                ..crate::config::PushConfig::default()
            };
            let capture = crate::push::test_sink::Capture::install();

            let sent = relay(
                &mut app,
                serde_json::json!({"kind": "send", "params": {"text": "build done", "caller_pane_id": "w1-1"}}),
            );
            let message = &sent["result"]["message"];
            assert_eq!(message["from"], format!("{PEER}/llm-opt"), "{sent}");
            assert_eq!(message["machine_label"], "Jerrys-Mac-Studio", "{sent}");
            let id = message["id"].as_str().unwrap();

            let alerts = capture.take().alerts;
            assert_eq!(alerts.len(), 1, "{alerts:?}");
            assert_eq!(alerts[0].title, "llm-opt on Jerrys-Mac-Studio");
            assert_eq!(alerts[0].body, "build done");

            let owner: serde_json::Value = serde_json::from_str(
                &app.handle_gram_list("owner".into(), GramListParams::default()),
            )
            .unwrap();
            let agent = relay(
                &mut app,
                serde_json::json!({"kind": "list", "params": {"caller_pane_id": "w1-1"}}),
            );
            for answer in [owner, agent] {
                let listed = answer["result"]["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|item| item["id"] == id)
                    .cloned()
                    .unwrap_or_else(|| panic!("the relayed Gram is listed: {answer}"));
                assert_eq!(listed["from"], format!("{PEER}/llm-opt"));
                assert_eq!(listed["machine_label"], "Jerrys-Mac-Studio", "{answer}");
            }
        }
    }
}
