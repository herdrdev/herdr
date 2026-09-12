//! Core-only authority for the existing reporter protocols. Registry data cannot grant it.

use crate::detect::Agent;

pub(super) fn policy(agent: Agent) -> ReportPolicy {
    use ReportAuthority::{FullLifecycle, None as NoAuthority, SessionIdentityOnly};
    use SessionReplacementEvent::{Branch, Clear, Compact, Fork, New, Resume, Select, Startup};
    use SessionReplacementPolicy as Replacement;

    match agent {
        Agent::Pi => ReportPolicy::official(
            "herdr:pi",
            FullLifecycle,
            false,
            Replacement::events(&[New, Resume, Fork]),
        ),
        Agent::Claude => ReportPolicy::official(
            "herdr:claude",
            NoAuthority,
            true,
            Replacement::events(&[Clear, Resume, Compact]),
        ),
        Agent::Codex => ReportPolicy::official(
            "herdr:codex",
            NoAuthority,
            true,
            Replacement::events(&[Startup, Clear, Resume, Compact]),
        ),
        Agent::Cursor => {
            ReportPolicy::official("herdr:cursor", NoAuthority, true, Replacement::NONE)
        }
        Agent::Devin => ReportPolicy::official("herdr:devin", NoAuthority, true, Replacement::NONE),
        Agent::Antigravity => ReportPolicy::official(
            "herdr:antigravity_cli",
            SessionIdentityOnly,
            false,
            Replacement::missing_event(),
        ),
        Agent::Omp => ReportPolicy::official(
            "herdr:omp",
            FullLifecycle,
            false,
            Replacement::events(&[Startup, New, Resume, Fork]),
        ),
        Agent::Mastracode => ReportPolicy::official(
            "herdr:mastracode",
            FullLifecycle,
            false,
            Replacement::events(&[Startup]),
        )
        .with_initial_lifecycle_session_replacement(),
        Agent::OpenCode => ReportPolicy::official(
            "herdr:opencode",
            FullLifecycle,
            false,
            Replacement::events(&[Select]).with_unsequenced_event(Select),
        ),
        Agent::GithubCopilot => {
            ReportPolicy::official("herdr:copilot", NoAuthority, true, Replacement::NONE)
        }
        Agent::Kimi => {
            ReportPolicy::official("herdr:kimi", FullLifecycle, false, Replacement::NONE)
        }
        Agent::Droid => ReportPolicy::official("herdr:droid", NoAuthority, true, Replacement::NONE),
        Agent::Grok => ReportPolicy::official("herdr:grok", NoAuthority, true, Replacement::NONE),
        Agent::Hermes => ReportPolicy::official(
            "herdr:hermes",
            SessionIdentityOnly,
            false,
            Replacement::events(&[Startup, New, Resume]),
        ),
        Agent::Kilo => {
            ReportPolicy::official("herdr:kilo", FullLifecycle, false, Replacement::NONE)
        }
        Agent::Qodercli => {
            ReportPolicy::official("herdr:qodercli", NoAuthority, true, Replacement::NONE)
        }
        Agent::Qwen => ReportPolicy::official(
            "herdr:qwen",
            SessionIdentityOnly,
            true,
            Replacement::events(&[Startup, Clear, Resume, Compact, Branch]),
        ),
        _ => ReportPolicy::NONE,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReportAuthority {
    None,
    FullLifecycle,
    SessionIdentityOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SessionReplacementEvent {
    Startup,
    Clear,
    Resume,
    Compact,
    Branch,
    New,
    Fork,
    Select,
}

impl SessionReplacementEvent {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "startup" => Some(Self::Startup),
            "clear" => Some(Self::Clear),
            "resume" => Some(Self::Resume),
            "compact" => Some(Self::Compact),
            "branch" => Some(Self::Branch),
            "new" => Some(Self::New),
            "fork" => Some(Self::Fork),
            "select" => Some(Self::Select),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SessionReplacementPolicy {
    events: &'static [SessionReplacementEvent],
    allows_missing_event: bool,
    unsequenced_event: Option<SessionReplacementEvent>,
}

impl SessionReplacementPolicy {
    pub(super) const NONE: Self = Self {
        events: &[],
        allows_missing_event: false,
        unsequenced_event: None,
    };

    pub(super) const fn events(events: &'static [SessionReplacementEvent]) -> Self {
        Self {
            events,
            allows_missing_event: false,
            unsequenced_event: None,
        }
    }

    pub(super) const fn missing_event() -> Self {
        Self {
            events: &[],
            allows_missing_event: true,
            unsequenced_event: None,
        }
    }

    pub(super) const fn with_unsequenced_event(mut self, event: SessionReplacementEvent) -> Self {
        self.unsequenced_event = Some(event);
        self
    }

    fn allows(self, event: Option<&str>) -> bool {
        match event {
            Some(event) => SessionReplacementEvent::parse(event)
                .is_some_and(|event| self.events.contains(&event)),
            None => self.allows_missing_event,
        }
    }

    fn allows_unsequenced(self, event: Option<&str>) -> bool {
        event
            .and_then(SessionReplacementEvent::parse)
            .is_some_and(|event| self.unsequenced_event == Some(event))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReportPolicy {
    official_source: Option<&'static str>,
    authority: ReportAuthority,
    reserved_native_state: bool,
    session_replacement: SessionReplacementPolicy,
    initial_lifecycle_report_replaces_session: bool,
}

impl ReportPolicy {
    pub(super) const NONE: Self = Self {
        official_source: None,
        authority: ReportAuthority::None,
        reserved_native_state: false,
        session_replacement: SessionReplacementPolicy::NONE,
        initial_lifecycle_report_replaces_session: false,
    };

    pub(super) const fn official(
        source: &'static str,
        authority: ReportAuthority,
        reserved_native_state: bool,
        session_replacement: SessionReplacementPolicy,
    ) -> Self {
        Self {
            official_source: Some(source),
            authority,
            reserved_native_state,
            session_replacement,
            initial_lifecycle_report_replaces_session: false,
        }
    }

    pub(super) const fn with_initial_lifecycle_session_replacement(mut self) -> Self {
        self.initial_lifecycle_report_replaces_session = true;
        self
    }

    pub(crate) const fn official_source(self) -> Option<&'static str> {
        self.official_source
    }

    pub(crate) const fn authority(self) -> ReportAuthority {
        self.authority
    }

    pub(crate) const fn reserves_native_state(self) -> bool {
        self.reserved_native_state
    }

    pub(crate) fn allows_session_replacement(self, event: Option<&str>) -> bool {
        self.session_replacement.allows(event)
    }

    pub(crate) fn allows_unsequenced_session_replacement(self, event: Option<&str>) -> bool {
        self.session_replacement.allows_unsequenced(event)
    }

    pub(crate) const fn initial_lifecycle_report_replaces_session(self) -> bool {
        self.initial_lifecycle_report_replaces_session
    }
}
