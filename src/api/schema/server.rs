use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct PingParams {}

crate::cli::projection::cli_params! {
    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct ServerLiveHandoffParams => ServerLiveHandoffProjection {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub import_exe: Option<String> => crate::cli::projection::Field::new(None).input(
            crate::cli::projection::Input::value("import-exe", "path", |slot, value| {
                *slot = Some(value.to_owned());
                Ok(())
            }).path()
        ),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub expected_protocol: Option<u32> => crate::cli::projection::Field::new(None).input(
            crate::cli::projection::Input::value("expected-protocol", "n", |slot, value| {
                *slot = Some(value.parse::<u32>().map_err(|_| {
                    crate::cli::projection::ParseError::Usage(format!("invalid expected protocol: {value}"))
                })?);
                Ok(())
            })
        ),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub expected_version: Option<String> => crate::cli::projection::Field::new(None).input(
            crate::cli::projection::Input::value("expected-version", "version", |slot, value| {
                *slot = Some(value.to_owned());
                Ok(())
            })
        ),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ServerSshAgentRegisterParams {
    /// Absolute remote-host agent socket. Registration lasts until this API connection closes.
    pub socket_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ServerCapabilities {
    pub live_handoff: bool,
    #[serde(default)]
    pub detached_server_daemon: bool,
    /// Stable client-owned endpoint generation supported by this server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_protocol_generation: Option<u32>,
    /// Whether this server supports explicit client-shell surface interest.
    #[serde(default)]
    pub surface_interest: bool,
    /// Whether this server supports endpoint health probes.
    #[serde(default)]
    pub health_check: bool,
    /// The daemon serves the persistent `pane.input.stream` write channel
    /// (issue #62). Clients feature-detect this before using it and otherwise
    /// fall back to per-call `pane.send_text` / `pane.send_input`.
    #[serde(default)]
    pub pane_input_stream: bool,
    /// The daemon serves `gram.upload.stream`, uploading a whole file over one
    /// connection instead of one connection per chunk. Clients feature-detect this
    /// and otherwise fall back to per-chunk `gram.upload_chunk`.
    #[serde(default)]
    pub gram_upload_stream: bool,
    /// The server can transactionally transfer visible session history between
    /// Claude Code, Codex, and OMP in the same logical pane.
    #[serde(default)]
    pub agent_session_transfer: bool,
    /// Native harnesses this daemon can use as session-transfer sources and
    /// destinations. Empty means the older Claude/Codex-only capability shape.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agent_session_transfer_harnesses: Vec<super::AgentSessionTransferHarness>,
    /// `events.subscribe` accepts `events_v2: true` (sequence-numbered lines,
    /// `lagged` and `heartbeat` control lines, per-entry `pane_not_found`
    /// rejection) and all-pane `pane.agent_status_changed` and
    /// `pane.turn_completed` entries without a `pane_id`.
    #[serde(default)]
    pub events_v2: bool,
    /// Supports connection-scoped `server.ssh_agent.register` on the local JSON API.
    #[serde(default)]
    pub ssh_agent_registration: bool,
    /// Supports `agent.forget` (remove an archived agent's record).
    #[serde(default)]
    pub agent_forget: bool,
    /// `herdr api-bridge --multi` forwards newline-delimited requests over one
    /// long-lived bridge process instead of one process per request.
    #[serde(default)]
    pub api_bridge_multi: bool,
    /// `pane.stream` reset keyframes carry `history_cursor` and `pane.history`
    /// pages older scrollback above it.
    #[serde(default)]
    pub pane_history: bool,
}
