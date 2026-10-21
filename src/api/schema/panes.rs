use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::cli::pane::bindings as binding;
use crate::cli::projection::{Field, Input};

use super::agents::AgentSessionInfo;
use super::common::{AgentStatus, PaneAgentState, ReadFormat, ReadSource, SplitDirection};
pub use crate::terminal::TurnOutcome;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum PaneRightClickTarget {
    #[default]
    Herdr,
    Pane,
}

crate::cli::projection::cli_params! {
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct PaneSplitParams => PaneSplitProjection {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub workspace_id: Option<String> => Field::new(None::<String>),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub target_pane_id: Option<String> => binding::target(true),
        pub direction: SplitDirection => Field::new(SplitDirection::Right).input(binding::split_direction()),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub ratio: Option<f32> => Field::new(None).input(binding::finite("ratio")),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub cwd: Option<String> => Field::new(None).input(binding::optional_text("cwd", "PATH").path()),
        #[serde(default)]
        pub focus: bool => Field::new(false).input(binding::boolean("focus", true)).input(binding::boolean("no-focus", false)),
        #[serde(default)]
        pub right_click: PaneRightClickTarget => Field::new(PaneRightClickTarget::Herdr).input(binding::right_click()),
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        pub env: HashMap<String, String> => Field::new(HashMap::new()).input(binding::environment()),
    }
}

crate::cli::projection::cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct PaneInputSetParams => PaneInputProjection {
        pub pane_id: String => binding::input_target(),
        pub right_click: PaneRightClickTarget => Field::new(PaneRightClickTarget::Herdr).input(binding::right_click().required()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneLinkActivateParams {
    pub pane_id: String,
    pub viewport_row: u16,
    pub col: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset_from_bottom: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PaneDirection {
    Left,
    Right,
    Up,
    Down,
}

crate::cli::projection::cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
    pub struct PaneSwapParams => PaneSwapProjection {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub pane_id: Option<String> => binding::target(false),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub direction: Option<PaneDirection> => Field::new(None).input(binding::optional_direction().configure(|arg| arg.conflicts_with_all(["source-pane", "target-pane"]))),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub source_pane_id: Option<String> => Field::new(None).input(binding::optional_text("source-pane", "ID").configure(|arg| arg.requires("target-pane"))),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub target_pane_id: Option<String> => Field::new(None).input(binding::optional_text("target-pane", "ID").configure(|arg| arg.requires("source-pane"))),
    }
}

crate::cli::projection::cli_params! {
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct PaneMoveParams => PaneMoveProjection {
        pub pane_id: String => Field::new(String::new()).input(binding::pane_id()),
        pub destination: PaneMoveDestination => binding::destination(),
        #[serde(default)]
        pub focus: bool => Field::new(true).input(binding::boolean("focus", true)).input(binding::boolean("no-focus", false)),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PaneMoveDestination {
    Tab {
        tab_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_pane_id: Option<String>,
        split: SplitDirection,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ratio: Option<f32>,
    },
    NewTab {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    NewWorkspace {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tab_label: Option<String>,
    },
}

crate::cli::projection::cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
    pub struct PaneZoomParams => PaneZoomProjection {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub pane_id: Option<String> => binding::target(true),
        #[serde(default)]
        pub mode: PaneZoomMode => Field::new(PaneZoomMode::Toggle)
            .input(Input::flag("toggle", |slot, _| { *slot = PaneZoomMode::Toggle; Ok(()) }).configure(|arg| arg.conflicts_with_all(["on", "off"])))
            .input(Input::flag("on", |slot, _| { *slot = PaneZoomMode::On; Ok(()) }).configure(|arg| arg.conflicts_with("off")))
            .input(Input::flag("off", |slot, _| { *slot = PaneZoomMode::Off; Ok(()) })),
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum PaneZoomMode {
    #[default]
    Toggle,
    On,
    Off,
}

crate::cli::projection::cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
    pub struct PaneLayoutParams => PaneLayoutProjection {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub pane_id: Option<String> => binding::target(false),
    }
}

crate::cli::projection::cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
    pub struct PaneProcessInfoParams => PaneProcessInfoProjection {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub pane_id: Option<String> => binding::target(false),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct LayoutExportParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LayoutApplyParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_label: Option<String>,
    #[serde(default)]
    pub focus: bool,
    pub root: LayoutNode,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LayoutSetSplitRatioParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    pub path: Vec<bool>,
    pub ratio: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LayoutDescription {
    pub workspace_id: String,
    pub tab_id: String,
    pub zoomed: bool,
    pub focused_pane_id: String,
    pub root: LayoutNode,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LayoutNode {
    Pane {
        #[serde(flatten)]
        pane: LayoutPane,
    },
    Split {
        direction: SplitDirection,
        ratio: f32,
        first: Box<LayoutNode>,
        second: Box<LayoutNode>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct LayoutPane {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
}

crate::cli::projection::cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct PaneNeighborParams => PaneNeighborProjection {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub pane_id: Option<String> => binding::target(false),
        pub direction: PaneDirection => Field::new(PaneDirection::Left).input(binding::direction()),
    }
}

crate::cli::projection::cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
    pub struct PaneEdgesParams => PaneEdgesProjection {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub pane_id: Option<String> => binding::target(false),
    }
}

crate::cli::projection::cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct PaneFocusDirectionParams => PaneFocusProjection {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub pane_id: Option<String> => binding::target(false),
        pub direction: PaneDirection => Field::new(PaneDirection::Left).input(binding::direction()),
    }
}

crate::cli::projection::cli_params! {
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct PaneResizeParams => PaneResizeProjection {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub pane_id: Option<String> => binding::target(false),
        pub direction: PaneDirection => Field::new(PaneDirection::Left).input(binding::direction()),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub amount: Option<f32> => Field::new(None).input(binding::finite("amount")),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneScrollParams {
    pub pane_id: String,
    pub offset_from_bottom: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneTextPoint {
    pub row: u32,
    pub col: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneSelectionReadParams {
    pub pane_id: String,
    pub anchor: PaneTextPoint,
    pub cursor: PaneTextPoint,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_revision: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PaneCopyMotion {
    LineEnd,
    FirstNonBlank,
    NextWordStart,
    PreviousWordStart,
    NextWordEnd,
    NextBigWordStart,
    PreviousBigWordStart,
    NextBigWordEnd,
    PreviousParagraph,
    NextParagraph,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneCopyMotionParams {
    pub pane_id: String,
    pub cursor: PaneTextPoint,
    pub motion: PaneCopyMotion,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_revision: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PaneCopySearchDirection {
    Forward,
    Backward,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneTextRange {
    pub start: PaneTextPoint,
    pub end: PaneTextPoint,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneCopySearchParams {
    pub pane_id: String,
    pub query: String,
    pub direction: PaneCopySearchDirection,
    pub cursor: PaneTextPoint,
    pub content_revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<PaneTextRange>,
}

/// Sets the real PTY window size (cols/rows) of a pane and takes or releases
/// geometry ownership so a remote client can drive the pane size. Distinct from
/// `pane.resize`, which changes the layout split ratio rather than the winsize.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneSetPtySizeParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    pub cols: u16,
    pub rows: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cell_width_px: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cell_height_px: Option<u32>,
    /// When true, take geometry ownership: the desktop TUI stops re-asserting
    /// layout-driven winsize for this pane. When false, release it so the TUI
    /// reclaims the size on its next render.
    #[serde(default)]
    pub lock: bool,
    /// Stable identity of the viewer taking this width lease. Send the SAME
    /// `viewer_id` on the viewer's `pane.stream` so the lease is dropped when
    /// that stream closes. Absent for legacy callers, whose lease then relies on
    /// the TTL backstop and explicit release only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewer_id: Option<String>,
    /// Optional width-lease TTL in milliseconds (1 .. 86_400_000). Backstop that
    /// drops the lease if the viewer disconnects ungracefully; defaults to a
    /// generous server value when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 86_400_000))]
    pub ttl_ms: Option<u64>,
    /// Internal: refuse `lock:true` unless this viewer has a `pane.stream` open
    /// on the pane (the guest gate).
    #[serde(skip)]
    #[schemars(skip)]
    pub(crate) require_stream: bool,
}

/// Internal: the viewer whose width leases to drop (guest gate, unix-only).
#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PanePtyLeaseReleaseParams {
    pub viewer_id: String,
}

crate::cli::projection::cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
    pub struct PaneListParams => PaneListProjection {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub workspace_id: Option<String> => Field::new(None).input(Input::value("workspace", "WORKSPACE_ID", |slot, value| { *slot = Some(crate::cli::normalize_workspace_id(value)); Ok(()) })),
    }
}

crate::cli::projection::cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
    pub struct PaneCurrentParams => PaneCurrentProjection {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub caller_pane_id: Option<String> => binding::target(false),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneTurnsParams {
    pub pane_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_epoch: Option<u64>,
}

crate::cli::projection::cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct PaneRenameParams => PaneRenameProjection {
        pub pane_id: String => Field::new(String::new()).input(binding::pane_id()),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub label: Option<String> => binding::label(),
    }
}

crate::cli::projection::cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct PaneSendTextParams => PaneSendTextProjection {
        pub pane_id: String => Field::new(String::new()).input(binding::pane_id()),
        pub text: String => Field::new(String::new()).input(binding::joined("text", "TEXT")),
    }
}

crate::cli::projection::cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct PaneSendKeysParams => PaneSendKeysProjection {
        pub pane_id: String => Field::new(String::new()).input(binding::pane_id()),
        pub keys: Vec<String> => Field::new(Vec::new()).input(binding::keys()),
    }
}

crate::cli::projection::cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct PaneSendInputParams => PaneRunProjection {
        pub pane_id: String => Field::new(String::new()).input(binding::pane_id()),
        #[serde(default, skip_serializing_if = "String::is_empty")]
        pub text: String => Field::new(String::new()).input(binding::joined("command", "COMMAND")),
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub keys: Vec<String> => Field::new(vec![String::from("Enter")]),
    }
}

/// Open params for the persistent `pane.input.stream` write channel. Frames on
/// the held-open connection are field-compatible with `PaneSendInputParams`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneInputStreamParams {
    pub pane_id: String,
}

fn default_include_history() -> bool {
    true
}

/// Subscription parameters for `pane.stream`, the persistent server->client raw
/// PTY byte firehose used by remote clients (the mobile app) to drive a live
/// VT emulator. `seq` on the wire is an absolute byte offset and `epoch`
/// identifies the runtime generation; both are carried so gap-free resume can
/// be added later without a wire change (v1 always re-seeds a fresh reset).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneStreamParams {
    pub pane_id: String,
    /// Whether `reset` keyframes carry `scrollback_lines` of history. Set it to
    /// `false` to opt out without dropping `scrollback_lines`.
    #[serde(default = "default_include_history")]
    pub include_history: bool,
    /// Absolute byte offset to resume from. Carried on the wire for a later
    /// gap-free resume; v1 ignores it and always sends a fresh reset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_from: Option<u64>,
    /// Runtime epoch the client last saw. Carried for resume; v1 always resyncs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epoch: Option<u64>,
    /// Maximum decoded bytes per `data` frame. Oversize deltas are split across
    /// frames. Defaults to a server-chosen chunk size when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_frame_bytes: Option<usize>,
    /// Number of normal-screen scrollback rows (at most 1000) every `reset`
    /// keyframe carries in `history_b64`, captured atomically with its screen.
    /// Omitted or `0` sends screen-only keyframes, as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scrollback_lines: Option<usize>,
    /// Stable identity of this viewer, echoed on its `pane.set_pty_size` so the
    /// viewer's width lease is dropped when this stream closes. The server
    /// carries it through to the close so the correct lease is released. Absent
    /// for legacy clients (their leases rely on the TTL backstop).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewer_id: Option<String>,
}

crate::cli::projection::cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct PaneReadParams => PaneReadProjection {
        pub pane_id: String => Field::new(String::new()).input(binding::pane_id()),
        pub source: ReadSource => Field::new(ReadSource::Recent).input(binding::read_source(true)),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub lines: Option<u32> => Field::new(None).input(binding::lines()),
        #[serde(default)]
        pub format: ReadFormat => binding::read_format(),
        #[serde(default = "super::default_true")]
        pub strip_ansi: bool => Field::new(true).input(binding::boolean("raw", false)),
        #[serde(skip)]
        #[schemars(skip)]
        pub(crate) intent: super::common::ReadIntent => Field::new(super::common::ReadIntent::Interactive),
    }
}

crate::cli::projection::cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct PaneReportAgentParams => PaneReportAgentProjection {
        pub pane_id: String => Field::new(String::new()).input(binding::pane_id()),
        pub source: String => Field::new(String::new()).input(binding::source()),
        pub agent: String => Field::new(String::new()).input(binding::text("agent", "LABEL").required()),
        pub state: PaneAgentState => Field::new(PaneAgentState::Unknown).input(binding::state()),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub message: Option<String> => Field::new(None).input(binding::optional_text("message", "TEXT")),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub seq: Option<u64> => Field::new(None).input(binding::number("seq", "N")),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub agent_session_id: Option<String> => Field::new(None).input(binding::optional_text("agent-session-id", "ID")),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub agent_session_path: Option<String> => Field::new(None).input(binding::optional_text("agent-session-path", "PATH").path()),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub agent_session_cursor: Option<String> => Field::new(None::<String>),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub agent_process_pid: Option<u32> => Field::new(None::<u32>),
        /// Command that resumes this agent's session after a Herdr restart. The
        /// first element must be a plain command name.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub resume_argv: Option<Vec<String>> => Field::new(None).input(binding::resume()),
    }
}

crate::cli::projection::cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct PaneReportAgentSessionParams => PaneReportSessionProjection {
        pub pane_id: String => Field::new(String::new()).input(binding::pane_id()),
        pub source: String => Field::new(String::new()).input(binding::source()),
        pub agent: String => Field::new(String::new()).input(binding::text("agent", "LABEL").required()),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub seq: Option<u64> => Field::new(None).input(binding::number("seq", "N")),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub agent_session_id: Option<String> => Field::new(None).input(binding::optional_text("agent-session-id", "ID")),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub agent_session_path: Option<String> => Field::new(None).input(binding::optional_text("agent-session-path", "PATH").path()),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub agent_session_cursor: Option<String> => Field::new(None).input(binding::optional_text("agent-session-cursor", "ID")),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub agent_process_pid: Option<u32> => Field::new(None).input(binding::agent_process_pid()),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub session_start_source: Option<String> => Field::new(None).input(binding::optional_text("session-start-source", "SOURCE")),
        /// Command that resumes this agent's session after a Herdr restart. The
        /// first element must be a plain command name.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub resume_argv: Option<Vec<String>> => Field::new(None).input(binding::resume()),
    }
}

crate::cli::projection::cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct PaneReportMetadataParams => PaneMetadataProjection {
        pub pane_id: String => Field::new(String::new()).input(binding::pane_id()),
        pub source: String => Field::new(String::new()).input(binding::source()),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub agent: Option<String> => Field::new(None).input(binding::optional_text("agent", "LABEL")),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub applies_to_source: Option<String> => Field::new(None).input(binding::optional_text("applies-to-source", "ID")),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub title: Option<String> => Field::new(None).input(binding::optional_text("title", "TEXT").configure(|arg| arg.conflicts_with("clear-title"))),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub display_agent: Option<String> => Field::new(None).input(binding::optional_text("display-agent", "TEXT").configure(|arg| arg.conflicts_with("clear-display-agent"))),
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        pub state_labels: HashMap<String, String> => Field::new(HashMap::new()).input(binding::state_label().configure(|arg| arg.conflicts_with("clear-state-labels"))),
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        #[schemars(schema_with = "super::common::metadata_token_patch_schema")]
        pub tokens: HashMap<String, Option<String>> => Field::new(HashMap::new()).input(binding::token()).input(binding::clear_token()),
        #[serde(default)]
        pub clear_title: bool => Field::new(false).input(binding::boolean("clear-title", true)),
        #[serde(default)]
        pub clear_display_agent: bool => Field::new(false).input(binding::boolean("clear-display-agent", true)),
        #[serde(default)]
        pub clear_state_labels: bool => Field::new(false).input(binding::boolean("clear-state-labels", true)),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub seq: Option<u64> => Field::new(None).input(binding::number("seq", "N")),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(range(min = 1, max = 86_400_000))]
        pub ttl_ms: Option<u64> => Field::new(None).input(binding::number("ttl-ms", "N")),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneClearAgentAuthorityParams {
    pub pane_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
}

crate::cli::projection::cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct PaneReleaseAgentParams => PaneReleaseProjection {
        pub pane_id: String => Field::new(String::new()).input(binding::pane_id()),
        pub source: String => Field::new(String::new()).input(binding::source()),
        pub agent: String => Field::new(String::new()).input(binding::text("agent", "LABEL").required()),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub seq: Option<u64> => Field::new(None).input(binding::number("seq", "N")),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneInfo {
    pub pane_id: String,
    pub terminal_id: String,
    pub workspace_id: String,
    pub tab_id: String,
    pub focused: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foreground_cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restore_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_title_stripped: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_agent: Option<String>,
    pub agent_status: AgentStatus,
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub input_pending: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_prompt_kind: Option<crate::detect::InputPromptKind>,
    /// Best-effort composer state. Older daemons omit this field; consumers
    /// must treat that as `unknown`, never as an empty composer.
    #[serde(default)]
    pub composer: ComposerInfo,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub state_labels: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    #[schemars(schema_with = "super::common::metadata_token_values_schema")]
    pub tokens: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session: Option<AgentSessionInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_completed_turn: Option<LastCompletedTurn>,
    /// Advisory hint only; use `pane.turns` replay as the completeness authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<u64>,
    /// Advisory hint only; use `pane.turns` replay as the completeness authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_epoch: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scroll: Option<PaneScrollInfo>,
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub alternate_screen: bool,
    pub revision: u64,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum ComposerState {
    Empty,
    DraftPresent,
    #[default]
    Unknown,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum ComposerProvenance {
    None,
    Human,
    Api,
    AgentPrompt,
    #[default]
    Unknown,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum ComposerRegionEvidence {
    Empty,
    Text,
    Missing,
    #[default]
    Unavailable,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum ComposerCursorEvidence {
    Draft,
    Suggestion,
    Conflict,
    #[default]
    Unavailable,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum ComposerStyleEvidence {
    Neutral,
    Conflict,
    #[default]
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ComposerEvidence {
    pub provenance: ComposerProvenance,
    pub region: ComposerRegionEvidence,
    pub cursor: ComposerCursorEvidence,
    pub style: ComposerStyleEvidence,
    pub frame_stable: bool,
}

impl Default for ComposerEvidence {
    fn default() -> Self {
        Self {
            provenance: ComposerProvenance::Unknown,
            region: ComposerRegionEvidence::Unavailable,
            cursor: ComposerCursorEvidence::Unavailable,
            style: ComposerStyleEvidence::Unavailable,
            frame_stable: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct ComposerInfo {
    pub state: ComposerState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    #[serde(default)]
    pub evidence: ComposerEvidence,
    /// True when a delayed prompt submission was withheld because the pane's
    /// occupant changed before the submitting key could be written. The text is
    /// in the composer and no turn started; the caller was already told the
    /// prompt was received, so this is the only place that fact surfaces.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub submit_abandoned: bool,
    /// Who put this text here, as distinct from `provenance`, which says only
    /// how it arrived. `provenance: agent_prompt` names a channel and was
    /// routinely read as naming a sender; that inference is what misattributed
    /// eleven keyboard-typed messages to the last API prompter (#31).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<ComposerAuthor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ComposerAuthor {
    /// Typed or pasted at the keyboard by a person.
    Human,
    /// Written by a client through the JSON API.
    ApiClient,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LastCompletedTurn {
    pub turn: u64,
    pub turn_epoch: u64,
    /// Unix timestamp in milliseconds.
    pub completed_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneTurnRecord {
    pub turn: u64,
    pub turn_epoch: u64,
    /// `completed` means the agent became idle normally; `aborted` means it
    /// became idle within 15 seconds of an Esc or Ctrl+C interrupt.
    pub outcome: TurnOutcome,
    /// Unix timestamp in milliseconds.
    pub completed_unix_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub message_truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneTurnsResult {
    pub pane_id: String,
    pub turn_epoch: u64,
    pub records: Vec<PaneTurnRecord>,
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oldest_available: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneScrollInfo {
    pub offset_from_bottom: u64,
    pub max_offset_from_bottom: u64,
    pub viewport_rows: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneProcessInfo {
    pub pane_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell_pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foreground_process_group_id: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tty: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub foreground_processes: Vec<PaneProcessInfoProcess>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneProcessInfoProcess {
    pub pid: u32,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argv0: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argv: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cmdline: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneSwapResult {
    pub changed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<PaneSwapReason>,
    pub source_pane_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_pane_id: Option<String>,
    pub focused_pane_id: String,
    pub layout: PaneLayoutSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PaneSwapReason {
    NoNeighbor,
    SamePane,
    NotFound,
    CrossTab,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneMoveResult {
    pub changed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<PaneMoveReason>,
    pub previous_pane_id: String,
    pub previous_workspace_id: String,
    pub previous_tab_id: String,
    pub pane: Box<PaneInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_layout: Option<Box<PaneLayoutSnapshot>>,
    pub target_layout: Box<PaneLayoutSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_workspace: Option<super::WorkspaceInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_tab: Option<super::TabInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_tab_id: Option<String>,
    pub focused_pane_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PaneMoveReason {
    SameTab,
    ZoomedTab,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneZoomResult {
    pub changed: bool,
    pub zoom_changed: bool,
    pub focus_changed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<PaneZoomReason>,
    pub pane_id: String,
    pub focused_pane_id: String,
    pub zoomed: bool,
    pub layout: PaneLayoutSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PaneZoomReason {
    SinglePane,
    AlreadyZoomed,
    AlreadyUnzoomed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneLayoutSnapshot {
    pub workspace_id: String,
    pub tab_id: String,
    pub zoomed: bool,
    pub area: PaneLayoutRect,
    pub focused_pane_id: String,
    pub panes: Vec<PaneLayoutPane>,
    pub splits: Vec<PaneLayoutSplit>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneLayoutRect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneLayoutPane {
    pub pane_id: String,
    pub focused: bool,
    pub rect: PaneLayoutRect,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneLayoutSplit {
    pub id: String,
    pub direction: SplitDirection,
    pub ratio: f32,
    pub rect: PaneLayoutRect,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneNeighborResult {
    pub pane_id: String,
    pub direction: PaneDirection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub neighbor_pane_id: Option<String>,
    pub layout: PaneLayoutSnapshot,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneEdgesResult {
    pub pane_id: String,
    pub left: bool,
    pub right: bool,
    pub up: bool,
    pub down: bool,
    pub layout: PaneLayoutSnapshot,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneFocusDirectionResult {
    pub changed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<PaneFocusDirectionReason>,
    pub source_pane_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused_pane_id: Option<String>,
    pub layout: PaneLayoutSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PaneFocusDirectionReason {
    NoNeighbor,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneResizeResult {
    pub changed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<PaneResizeReason>,
    pub pane_id: String,
    pub focused_pane_id: String,
    pub layout: PaneLayoutSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PaneResizeReason {
    Unchanged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneReadResult {
    pub pane_id: String,
    pub workspace_id: String,
    pub tab_id: String,
    pub source: ReadSource,
    pub format: ReadFormat,
    pub text: String,
    pub revision: u64,
    pub truncated: bool,
}

/// Inclusive display-cell columns on a pane's current viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneLinkRegion {
    pub row: u16,
    pub start_col: u16,
    pub end_col: u16,
}
