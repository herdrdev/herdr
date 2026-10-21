use serde::{Deserialize, Serialize};

pub(super) fn metadata_token_patch_schema(
    _generator: &mut schemars::SchemaGenerator,
) -> schemars::Schema {
    schemars::json_schema!({
        "type": "object",
        "maxProperties": 16,
        "propertyNames": { "pattern": "^[A-Za-z0-9_-]{1,32}$" },
        "additionalProperties": { "type": ["string", "null"] }
    })
}

pub(super) fn metadata_token_values_schema(
    _generator: &mut schemars::SchemaGenerator,
) -> schemars::Schema {
    schemars::json_schema!({
        "type": "object",
        "maxProperties": 32,
        "propertyNames": { "pattern": "^[A-Za-z0-9_-]{1,32}$" },
        "additionalProperties": { "type": "string" }
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct EmptyParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkspaceTarget {
    pub workspace_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneTarget {
    pub pane_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProductAnnouncementDismissParams {
    pub version: String,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ReleaseNotesDismissParams {
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TabTarget {
    pub tab_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentTarget {
    pub target: String,
}

crate::cli::projection::cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct ClientWindowTitleSetParams => ClientWindowTitleSetProjection {
        pub title: String => crate::cli::projection::Field::new(String::new()).input(
            crate::cli::projection::Input::positional("title", 0, |title, value| {
                *title = value.to_owned();
                Ok(())
            }).value_name("TITLE").verbatim()
        ),
    }
}

/// Updates whether the requesting client shell receives and controls pane presentation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ClientShellSurfaceSetParams {
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SplitDirection {
    Right,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReadSource {
    Visible,
    Recent,
    RecentUnwrapped,
    Detection,
}

pub(crate) const READ_SOURCES: &[crate::cli::projection::Choice<ReadSource>] = &[
    crate::cli::projection::Choice::new("visible", ReadSource::Visible),
    crate::cli::projection::Choice::new("recent", ReadSource::Recent),
    crate::cli::projection::Choice::new("recent-unwrapped", ReadSource::RecentUnwrapped),
    crate::cli::projection::Choice::hidden("recent_unwrapped", ReadSource::RecentUnwrapped),
    crate::cli::projection::Choice::new("detection", ReadSource::Detection),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ReadIntent {
    #[default]
    Interactive,
    Passive,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum ReadFormat {
    #[default]
    Text,
    Ansi,
}

pub(crate) const READ_FORMATS: &[crate::cli::projection::Choice<ReadFormat>] = &[
    crate::cli::projection::Choice::new("text", ReadFormat::Text),
    crate::cli::projection::Choice::new("ansi", ReadFormat::Ansi),
];

crate::cli::projection::cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct NotificationShowParams => NotificationShowProjection {
        pub title: String => notification_cli::title(),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub body: Option<String> => notification_cli::body(),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub position: Option<crate::config::ToastHerdrPosition> => notification_cli::position(),
        #[serde(default, skip_serializing_if = "NotificationShowSound::is_none")]
        pub sound: NotificationShowSound => notification_cli::sound(),
    }
}

mod notification_cli {
    use super::NotificationShowSound;
    use crate::cli::projection::{Field, Input, ParseError};
    use crate::config::ToastHerdrPosition;

    const POSITIONS: [(&str, ToastHerdrPosition); 4] = [
        ("top-left", ToastHerdrPosition::TopLeft),
        ("top-right", ToastHerdrPosition::TopRight),
        ("bottom-left", ToastHerdrPosition::BottomLeft),
        ("bottom-right", ToastHerdrPosition::BottomRight),
    ];
    const SOUNDS: [(&str, NotificationShowSound); 3] = [
        ("none", NotificationShowSound::None),
        ("done", NotificationShowSound::Done),
        ("request", NotificationShowSound::Request),
    ];

    pub(super) fn title() -> Field<String> {
        Field::new(String::new()).input(
            Input::positional("title", 0, |slot, value| {
                *slot = value.to_owned();
                Ok(())
            })
            .value_name("TITLE")
            .verbatim(),
        )
    }

    pub(super) fn body() -> Field<Option<String>> {
        Field::new(None)
            .input(crate::cli::workspace_bindings::optional_text("body", "TEXT").order(0))
    }

    pub(super) fn position() -> Field<Option<ToastHerdrPosition>> {
        Field::new(None).input(Input::value("position", "POSITION", |slot, value| {
            *slot = Some(POSITIONS.iter().find(|(name, _)| *name == value).map(|(_, value)| *value)
                .ok_or_else(|| ParseError::Usage(format!("invalid position: {value} (expected top-left, top-right, bottom-left, or bottom-right)")))?);
            Ok(())
        }).configure(|arg| arg.value_parser(clap::builder::PossibleValuesParser::new(POSITIONS.iter().map(|(name, _)| *name)))).order(1))
    }

    pub(super) fn sound() -> Field<NotificationShowSound> {
        Field::new(NotificationShowSound::None).input(
            Input::value("sound", "SOUND", |slot, value| {
                *slot = SOUNDS
                    .iter()
                    .find(|(name, _)| *name == value)
                    .map(|(_, value)| *value)
                    .ok_or_else(|| {
                        ParseError::Usage(format!(
                            "invalid sound: {value} (expected none, done, or request)"
                        ))
                    })?;
                Ok(())
            })
            .configure(|arg| {
                arg.value_parser(clap::builder::PossibleValuesParser::new(
                    SOUNDS.iter().map(|(name, _)| *name),
                ))
            })
            .order(2),
        )
    }
}

/// Register (or update) a mobile device for remote push notifications.
///
/// Field names match the iOS client's `RegisterDeviceParams` contract. The
/// `notify_*` preferences default to `false` so a partial payload never fails
/// deserialization; the client sends explicit values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NotificationsRegisterDeviceParams {
    pub device_token: String,
    pub platform: String,
    #[serde(default)]
    pub notify_needs_input: bool,
    #[serde(default)]
    pub notify_dies: bool,
    #[serde(default)]
    pub notify_finishes: bool,
    #[serde(default)]
    pub notify_gram: bool,
    /// Public pane ids the owner has muted on this device. Pushes for a muted
    /// pane are skipped for this device. The client owns the set and sends the
    /// full list on each change; absent on older clients.
    #[serde(default)]
    pub muted_panes: Vec<String>,
    /// Sealed `hpr1.` capability from the HerdrUp push relay, used when the
    /// daemon delivers through the relay. Absent on older clients.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay_capability: Option<String>,
}

/// Stop pushing to a device registered with `notifications.register_device`.
/// A guest removes only its own registration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NotificationsUnregisterDeviceParams {
    pub device_token: String,
}

/// Register / unregister an iOS Live Activity push token for BACKGROUND widget updates.
/// The field name matches the iOS client's `RegisterActivityParams` contract; the same
/// struct serves both `notifications.register_activity` and `.unregister_activity`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NotificationsRegisterActivityParams {
    pub activity_push_token: String,
    /// Sealed `hpr1.` capability from the HerdrUp push relay for this activity
    /// token. Ignored by `.unregister_activity`; absent on older clients.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay_capability: Option<String>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum NotificationShowSound {
    #[default]
    None,
    Done,
    Request,
}

impl NotificationShowSound {
    pub fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NotificationShowReason {
    Shown,
    Disabled,
    RateLimited,
    NoForegroundClient,
    Busy,
}

/// Readiness of remote push on this daemon, reported by `notifications.status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NotificationsStatusState {
    /// This daemon cannot deliver push (no-session mode has no device registry).
    Unsupported,
    /// `push.mode = "off"`.
    Off,
    /// Direct mode without a complete APNs key config, or relay/auto mode with no
    /// capability-bearing device registered yet.
    Unconfigured,
    DirectReady,
    RelayReady,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ClientWindowTitleReason {
    Set,
    Cleared,
    NoForegroundClient,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PaneAgentState {
    Idle,
    Working,
    Blocked,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Idle,
    Working,
    Blocked,
    Done,
    Unknown,
}

pub(crate) fn default_true() -> bool {
    true
}
