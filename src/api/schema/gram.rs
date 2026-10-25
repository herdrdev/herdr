use serde::{Deserialize, Serialize};

/// Direction of a gram message on the wire. Mirrors
/// [`crate::persist::gram::GramDirection`]; the handler maps between them so the
/// storage record and the public contract can evolve independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GramDirection {
    AgentToOwner,
    OwnerToAgent,
}

/// `gram.send` — an agent sends the owner a push-notified message.
///
/// The sender identity is `from` when provided; otherwise it is resolved
/// server-side from `caller_pane_id` (the agent's `HERDR_PANE_ID`) to the agent's
/// name (else the pane's public id). The agent name is the durable identity — it
/// survives a restart or live-handoff — but it is a name: it can be renamed,
/// cleared, or reused, so attribution and the "sent by me" view follow the
/// identity as it stands, not a fixed token. An explicit `from` overrides
/// attribution entirely. `text` is capped server-side (~8 KiB); send large
/// content as a file, not a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GramSendParams {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_pane_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// An optional file to attach, previously uploaded in chunks via
    /// `gram.upload_chunk` under `file.upload_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<GramFileUpload>,
}

/// A staged file to attach to a `gram.send`/`gram.post`. Its bytes must already be
/// uploaded in chunks via `gram.upload_chunk` under this `upload_id`; the handler
/// assembles them onto the newly minted message and clears the staging file.
/// `name` is the display name (sanitized to a safe basename server-side) and
/// `mime` is an advisory content type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GramFileUpload {
    pub upload_id: String,
    pub name: String,
    #[serde(default)]
    pub mime: String,
    /// Expected digest of the source bytes. Required by the cross-machine relay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

/// `gram.post` — the owner posts a message to agents (from the app).
///
/// `to: Some(agent)` addresses one agent directly by its unique agent name (not
/// grabbable); the name must match a live agent or the call is rejected. `to:
/// None` posts to the shared grab-queue any agent can claim. `text` is capped
/// server-side (~8 KiB).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GramPostParams {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// An optional file to attach, previously uploaded in chunks via
    /// `gram.upload_chunk` under `file.upload_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<GramFileUpload>,
    /// Internal only: the guest gate's sender label. Never read from the wire.
    #[serde(skip)]
    #[schemars(skip)]
    pub from: Option<String>,
}

/// `gram.list` — read messages. The audience is chosen by `caller_pane_id`:
/// omit it for the owner view (everything); supply it for that pane's agent view
/// (its direct items, the shared ungrabbed queue, its own grabs, and its own sent
/// items). A `caller_pane_id` that names no live pane is an error, not a
/// fall-through to the owner view. `unread_only` is an owner-view filter and is
/// rejected when `caller_pane_id` is present.
///
/// `limit`/`before_id` page the answer; omitting both returns the whole filtered
/// list, exactly as before they existed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct GramListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_pane_id: Option<String>,
    /// Restrict to the shared, still-ungrabbed queue (either audience).
    #[serde(default)]
    pub only_queue: bool,
    /// Owner view only: restrict to unread agent->owner messages.
    #[serde(default)]
    pub unread_only: bool,
    /// Conditional fetch: the `digest` from a previous `gram.list` answer. When it
    /// still matches, the reply is `gram_list_unchanged` — store id and digest only,
    /// no messages — so a polling client pays a few hundred bytes instead of the whole
    /// store. The app polls every 6s over one SSH channel, where a full owner view is
    /// ~900 KB for ~870 messages; re-sending that unchanged payload is what made the
    /// inbox slow to open and starved the channel everything else shares.
    ///
    /// The digest answers a HEAD poll and is computed over the whole filtered list,
    /// so it stays cheap and meaningful for a paging client too. It is ignored when
    /// `before_id` is present: an older page is requested explicitly, so there is
    /// nothing the client could already hold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub if_unchanged_digest: Option<String>,
    /// Maximum messages to return, counted from the NEWEST end of the filtered
    /// newest-first list. Absent means no limit. Clamped server-side to
    /// `GRAM_LIST_MAX_LIMIT`; `Some(0)` is rejected rather than answered with an
    /// empty page, because a client asking for nothing is a bug, not a request.
    ///
    /// Paging exists for the initial open: the owner view is ~900 KB for ~870
    /// messages over the one SSH channel everything else shares, and the reader
    /// only ever sees the newest screenful first. Search, Read-all and the unread
    /// badge survive a windowed client because `unread_count` is reported over the
    /// full filtered list, not the page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    /// Cursor: return only messages strictly OLDER than this id in the newest-first
    /// order. An id absent from the filtered list is rejected rather than treated as
    /// "start at the head" — a stale cursor that silently fell back would re-deliver
    /// page 1 forever while the reader scrolled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_id: Option<String>,
}

/// `gram.grab` — an agent claims a shared-queue item. The claim is first-wins and
/// atomic at the storage layer, so no two agents can ever hold the same item —
/// this holds regardless of identity. The claimant label is `grabbed_by` when
/// provided, otherwise resolved from `caller_pane_id` to the agent's identity;
/// that label carries the same name-semantics as `gram.send`'s `from` (a rename
/// or reuse moves which items show as "mine", but never lets a second agent
/// claim one). Fails if the item is missing, not a shared item, or already
/// grabbed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GramGrabParams {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_pane_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grabbed_by: Option<String>,
}

/// `gram.mark_read` — the owner marks agent->owner messages read: `id`, `ids`
/// or both. An unknown id marks nothing and answers `not_found`. A guest marks
/// its own read state only, never the owner's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GramMarkReadParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ids: Vec<String>,
}

impl GramMarkReadParams {
    /// Every id named, `id` first.
    pub fn targets(&self) -> impl Iterator<Item = &str> {
        self.id.iter().chain(&self.ids).map(String::as_str)
    }
}

/// `gram.delete` — remove a message from the store for good.
///
/// Deletion is deliberately destructive: the record (and any attached file bytes)
/// are gone, which is what makes gram safe for a short-lived secret like a
/// temporary API key — send it, use it, delete it. Authority follows the caller:
/// the owner's app sends no `caller_pane_id` and may delete any message; an agent
/// supplies its `caller_pane_id` and may delete only a message it is involved in
/// (one it sent, one addressed to it, or one it grabbed), else the call is
/// rejected. A `caller_pane_id` that names no live pane is an error, not a
/// fall-through to owner authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GramDeleteParams {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_pane_id: Option<String>,
}

/// `gram.upload_chunk` — append one chunk of a file being uploaded.
///
/// Files are sent in chunks because a single request is size-capped (the app's SSH
/// path near 65 KiB, the daemon at 1 MiB). `upload_id` groups the chunks of one
/// file; `offset` is the byte position this chunk starts at — the current staged
/// size — so a dropped or reordered chunk is caught, and `offset: 0` (re)starts the
/// upload. Attach the assembled file by passing the same `upload_id` in a
/// `gram.send`/`gram.post` `file`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GramUploadChunkParams {
    pub upload_id: String,
    pub offset: u64,
    pub data_base64: String,
}

/// `gram.upload.discard` — drop a staged upload that will never be attached, so
/// its bytes stop counting against the shared staging budget before the 24 h
/// sweep would reclaim them. Idempotent: `discarded` is `false` when no staged
/// upload has that id (never uploaded, already discarded, or already consumed by
/// a `gram.send`/`gram.post`), which is not an error. A guest's `upload_id` is
/// scoped to its own namespace, so a guest can discard only its own uploads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GramUploadDiscardParams {
    pub upload_id: String,
}

/// `gram.upload.stream` — open a streaming upload channel for one file.
///
/// The per-chunk `gram.upload_chunk` method costs one API connection per chunk, and
/// over the app's SSH transport one process spawn per chunk (a 100 MB file is ~2100).
/// This opens ONE connection, acks it, then reads newline-delimited chunk frames on
/// the same connection until EOF. Chunks still land through `gram_files::append_chunk`
/// under `upload_id`, and the file is still attached by passing the same `upload_id`
/// in a later `gram.send`/`gram.post` `file` — so nothing downstream changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GramUploadStreamParams {
    pub upload_id: String,
}

/// `gram.get_file` — legacy one-response download on the local socket. The
/// bytes are inline base64 and can be as large as 100 MiB. For a remote relay,
/// and for bounded new clients, use `gram.get_file_chunk` instead. Owner (no
/// caller pane) may download any file; an agent may only download one it can see.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GramGetFileParams {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_pane_id: Option<String>,
}
/// Bounded file read. A response contains at most 512 KiB of decoded bytes.
/// `offset == size` returns an empty chunk; a deleted file returns `not_found`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GramGetFileChunkParams {
    pub id: String,
    pub offset: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_pane_id: Option<String>,
}

/// Internal gateway envelope. The reverse SSH gateway overwrites `peer_alias`
/// from its pinned saved-machine route; it must never trust the wire value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GramRelayParams {
    pub peer_alias: String,
    pub call: GramRelayCall,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", content = "params", rename_all = "snake_case")]
pub enum GramRelayCall {
    Send(GramSendParams),
    List(GramListParams),
    UploadChunk(GramUploadChunkParams),
    UploadDiscard(GramUploadDiscardParams),
    GetFileChunk(GramGetFileChunkParams),
    Delete(GramDeleteParams),
    /// A HerdrUp guest's post to an agent on the relaying machine.
    Post(GramRelayPostParams),
}

/// A HerdrUp guest's post relayed by the machine that serves the guest: `to`
/// must name one of that machine's agents, and the message is labeled
/// `<guest> (via HerdrUp)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GramRelayPostParams {
    pub text: String,
    pub to: String,
    /// The guest's name, `^[A-Za-z0-9][A-Za-z0-9._-]{0,31}$`.
    pub guest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<GramFileUpload>,
}
/// File attached to a gram message, as returned to clients. Metadata only — fetch
/// the bytes with `gram.get_file`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GramFileInfo {
    pub name: String,
    pub size: u64,
    pub mime: String,
    pub sha256: String,
}

/// A gram message as returned to clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GramMessageInfo {
    pub id: String,
    pub direction: GramDirection,
    pub from: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grabbed_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grabbed_unix_ms: Option<u64>,
    pub created_unix_ms: u64,
    #[serde(default)]
    pub read_by_owner: bool,
    /// Metadata for an attached file, or absent. Fetch the bytes with
    /// `gram.get_file`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<GramFileInfo>,
    /// The attachment this message had before the store's attachment budget expired
    /// it: show what was there; there are no bytes to fetch. Absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expired_file: Option<GramFileInfo>,
    /// Install-stable identity of the daemon/store that wrote this message (see
    /// [`crate::persist::gram::GramItem::origin_id`]). Stable across daemon
    /// restarts; empty for messages written by an older build.
    #[serde(default)]
    pub origin_id: String,
    /// Display label of the federated machine a relayed Gram came from, when
    /// `from` is `<alias>/<name>` and that machine has a label: the same label
    /// the machine's agents carry as `machine_label`. Resolved when read, so it
    /// follows a rename and covers Grams stored before it. Absent for local
    /// Grams, for an alias without a label, and from older daemons.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_label: Option<String>,
}

/// Where an effective Gram relay setting came from.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum GramRelaySource {
    /// Disabled: nothing configured, or a conflict.
    #[default]
    None,
    /// `[gram_relay]` in config.toml (also when an equal legacy variable is set).
    Config,
    /// The deprecated legacy environment variable alone.
    Environment,
}

/// Whether the role's legacy environment variable is set in the daemon process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GramRelayEnvironment {
    Present,
    Absent,
}

impl GramRelayEnvironment {
    pub(crate) fn from_present(present: bool) -> Self {
        if present {
            Self::Present
        } else {
            Self::Absent
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GramRelayErrorCode {
    /// Config and the legacy environment variable differ; the role is disabled.
    Conflict,
    /// The last reload's `[gram_relay]` section was refused; the previous
    /// effective setting is kept.
    InvalidConfig,
}

/// One consented saved peer and its supervised reverse gateway.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GramRelayPeerStatus {
    pub alias: String,
    pub gateway: super::GramGatewayState,
}

/// Coordinator role: which saved peers may relay Gram.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GramRelayCoordinatorStatus {
    /// `gram_relay.peers` from config; `null` when unset.
    pub configured: Option<Vec<String>>,
    /// Legacy `HERDR_GRAM_RELAY_PEERS`.
    pub environment: GramRelayEnvironment,
    /// Peers actually allowed; `null` when disabled.
    pub effective: Option<Vec<String>>,
    pub source: GramRelaySource,
    pub error: Option<GramRelayErrorCode>,
    pub message: Option<String>,
    /// Gateway state of every effective peer.
    pub peers: Vec<GramRelayPeerStatus>,
}

/// Remote role: where local Gram calls are forwarded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GramRelayRemoteStatus {
    /// `gram_relay.coordinator_machine_id` from config; `null` when unset.
    pub configured_coordinator_machine_id: Option<String>,
    /// Reverse socket derived from the configured coordinator and this install.
    pub configured_socket: Option<String>,
    /// Legacy `HERDR_GRAM_REVERSE_SOCKET`.
    pub environment: GramRelayEnvironment,
    /// Socket Gram calls go to; `null` when disabled.
    pub effective_socket: Option<String>,
    pub source: GramRelaySource,
    pub error: Option<GramRelayErrorCode>,
    pub message: Option<String>,
    /// Whether the effective socket accepts a connection now; `null` when disabled.
    pub accepting: Option<bool>,
}
