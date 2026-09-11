use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::agents::session::{ReportReferencePreference, SessionProfile};

const MAX_SESSION_ID_LEN: usize = 512;
const MAX_SESSION_PATH_LEN: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSessionRef {
    pub kind: AgentSessionRefKind,
    pub value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentSessionRefKind {
    Id,
    Path,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentResumePlan {
    pub agent: String,
    pub argv: Vec<String>,
    pub dedupe_key: String,
    /// Pinned from the same immutable registry snapshot as `argv`.
    pub strict_input_readiness: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedAgentSession {
    pub source: String,
    pub agent: String,
    pub session_ref: AgentSessionRef,
}

/// Instructions captured from a single registry generation, never inferred from an executable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinnedAgentResumeRecipe {
    pub agent: String,
    pub executable: String,
    pub strategy: String,
    pub token: String,
    pub accepted_references: Vec<AgentSessionRefKind>,
    pub preferred_reference: AgentSessionRefKind,
}

/// Live capability is distinct from saved conversation metadata. `recipe: None`
/// means this acquired process has no resume capability, not "look it up later".
#[derive(Debug, Clone)]
pub(crate) struct LiveAgentResumeBinding {
    pub agent: crate::detect::Agent,
    pub recipe: Option<PinnedAgentResumeRecipe>,
    pub process: Option<(u32, crate::platform::ForegroundProcess)>,
    pub process_identity: Option<crate::platform::ProcessIdentity>,
    pub observed_at: std::time::Instant,
    pub managed_admission: bool,
    pub report_proof: Option<(crate::platform::ProcessIdentity, AgentSessionRef, bool)>,
}

impl PinnedAgentResumeRecipe {
    pub(crate) fn unavailable(agent: &str) -> Self {
        Self {
            agent: agent.into(),
            executable: String::new(),
            strategy: "unavailable".into(),
            token: String::new(),
            accepted_references: Vec::new(),
            preferred_reference: AgentSessionRefKind::Id,
        }
    }

    pub(crate) fn select_report_reference(
        &self,
        id: Option<String>,
        path: Option<String>,
    ) -> Option<AgentSessionRef> {
        let id = id
            .and_then(AgentSessionRef::id)
            .filter(|_| self.accepted_references.contains(&AgentSessionRefKind::Id));
        let path = path.and_then(AgentSessionRef::path).filter(|_| {
            self.accepted_references
                .contains(&AgentSessionRefKind::Path)
        });
        match self.preferred_reference {
            AgentSessionRefKind::Id => id,
            AgentSessionRefKind::Path => path.or(id),
        }
    }

    pub(crate) fn capture(profile: &crate::agents::AgentProfile) -> Option<Self> {
        use crate::agents::source::ResumeStrategy;
        let session = profile.session()?;
        Some(Self {
            agent: profile.canonical_id().into(),
            executable: profile.launch().executable().into(),
            strategy: match session.strategy {
                ResumeStrategy::SeparateFlag => "separate_flag",
                ResumeStrategy::JoinedFlag => "joined_flag",
                ResumeStrategy::Subcommand => "subcommand",
            }
            .into(),
            token: session.token.clone(),
            accepted_references: [AgentSessionRefKind::Id, AgentSessionRefKind::Path]
                .into_iter()
                .filter(|kind| session_profile_accepts_kind(session, *kind))
                .collect(),
            preferred_reference: match session.report_preference() {
                ReportReferencePreference::IdOnly => AgentSessionRefKind::Id,
                ReportReferencePreference::AbsolutePathThenId => AgentSessionRefKind::Path,
            },
        })
    }
}

pub(crate) fn recipe_for_report(source: &str, agent: &str) -> Option<PinnedAgentResumeRecipe> {
    let registry = crate::agents::registry();
    let (profile, _) = registry.session_profile_for_exact_report_pair(source, agent)?;
    PinnedAgentResumeRecipe::capture(profile)
}

/// Metadata is retained even when its package disappears. Only planning grants execution.
pub(crate) fn retained_snapshot_session(
    source: &str,
    agent: &str,
    kind: AgentSessionRefKind,
    value: &str,
) -> Option<PersistedAgentSession> {
    crate::detect::Agent::parse(agent).ok()?;
    if source != "herdr:launch" && !crate::agents::bundled_report_pair(source, agent) {
        return None;
    }
    Some(PersistedAgentSession {
        source: source.into(),
        agent: agent.into(),
        session_ref: match kind {
            AgentSessionRefKind::Id => AgentSessionRef::id(value)?,
            AgentSessionRefKind::Path => AgentSessionRef::path(value)?,
        },
    })
}

pub(crate) fn pinned_plan(
    registry: &crate::agents::RegistrySnapshot,
    session: &PersistedAgentSession,
    pinned: Option<&PinnedAgentResumeRecipe>,
) -> Result<AgentResumePlan, &'static str> {
    let profile = registry
        .profile_by_id(&session.agent)
        .ok_or("agent package is missing")?;
    if !profile.is_startable() {
        return Err("agent package is not startable");
    }
    let active = PinnedAgentResumeRecipe::capture(profile).ok_or("agent resume is unavailable")?;
    let baseline;
    let expected = match pinned {
        Some(pinned) => pinned,
        None => {
            baseline = crate::agents::bundled_profile(&session.agent)
                .and_then(PinnedAgentResumeRecipe::capture)
                .ok_or("unpinned session is not a bundled agent")?;
            &baseline
        }
    };
    if &active != expected {
        return Err("agent resume recipe changed; explicit launch required");
    }
    if !active
        .accepted_references
        .contains(&session.session_ref.kind)
    {
        return Err("agent resume reference kind is unsupported");
    }
    Ok(AgentResumePlan {
        agent: session.agent.clone(),
        argv: profile
            .session()
            .ok_or("agent resume is unavailable")?
            .argv(&active.executable, &session.session_ref.value),
        dedupe_key: dedupe_key(&session.source, &session.agent, &session.session_ref),
        strict_input_readiness: crate::detect::manifest::requires_screen_visible_idle(
            registry,
            profile.legacy_agent(),
        ),
    })
}

impl AgentSessionRef {
    pub fn id(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        valid_session_id(&value).then_some(Self {
            kind: AgentSessionRefKind::Id,
            value,
        })
    }

    pub fn path(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        valid_session_path(&value).then_some(Self {
            kind: AgentSessionRefKind::Path,
            value,
        })
    }
}

#[cfg(test)]
pub fn session_ref_from_report(
    source: &str,
    agent: &str,
    agent_session_id: Option<String>,
    agent_session_path: Option<String>,
) -> Option<AgentSessionRef> {
    let registry = crate::agents::registry();
    let (_, session) = registry.session_profile_for_exact_report_pair(source, agent)?;

    match session.report_preference() {
        ReportReferencePreference::IdOnly => agent_session_id.and_then(AgentSessionRef::id),
        ReportReferencePreference::AbsolutePathThenId => agent_session_path
            .and_then(AgentSessionRef::path)
            .or_else(|| agent_session_id.and_then(AgentSessionRef::id)),
    }
}

#[cfg(test)]
pub fn persisted_session_from_launch_args(
    agent: crate::detect::Agent,
    args: &[String],
) -> Option<PersistedAgentSession> {
    let registry = crate::agents::registry();
    persisted_session_from_profile_launch_args(registry.profile_by_id(agent.as_str())?, args)
}

pub(crate) fn persisted_session_from_profile_launch_args(
    profile: &crate::agents::AgentProfile,
    args: &[String],
) -> Option<PersistedAgentSession> {
    use crate::agents::source::ResumeStrategy;
    let session = profile.session()?;
    let value = match (session.strategy, args) {
        (ResumeStrategy::SeparateFlag | ResumeStrategy::Subcommand, [token, value])
            if token == &session.token =>
        {
            value.as_str()
        }
        (ResumeStrategy::JoinedFlag, [arg]) => arg.strip_prefix(&session.token)?,
        _ => return None,
    };
    if value.starts_with('-') {
        return None;
    }
    let session_ref = if session.accepts_path() {
        AgentSessionRef::path(value).or_else(|| {
            session
                .accepts_id()
                .then(|| AgentSessionRef::id(value))
                .flatten()
        })?
    } else if session.accepts_id() {
        AgentSessionRef::id(value)?
    } else {
        return None;
    };
    // Preserve core builtin ownership semantics; packages cannot declare trusted report pairs.
    let builtin_source = if profile.canonical_id() == "agy" {
        "herdr:antigravity_cli".into()
    } else {
        format!("herdr:{}", profile.canonical_id())
    };
    let source = if crate::agents::bundled_report_pair(&builtin_source, profile.canonical_id()) {
        builtin_source
    } else {
        "herdr:launch".into()
    };
    Some(PersistedAgentSession {
        source,
        agent: profile.canonical_id().into(),
        session_ref,
    })
}

pub fn normalize_session_start_source(value: Option<String>) -> Option<String> {
    match value.as_deref().map(str::trim) {
        Some(
            source @ ("startup" | "resume" | "clear" | "compact" | "branch" | "new" | "fork"
            | "select"),
        ) => Some(source.to_string()),
        _ => None,
    }
}

pub fn is_reserved_native_state_source(source: &str, agent: &str) -> bool {
    crate::agents::registry().is_reserved_native_state_source(source, agent)
}

fn session_profile_accepts_kind(session: &SessionProfile, kind: AgentSessionRefKind) -> bool {
    match kind {
        AgentSessionRefKind::Id => session.accepts_id(),
        AgentSessionRefKind::Path => session.accepts_path(),
    }
}

#[cfg(test)]
pub fn session_ref_from_snapshot(
    source: &str,
    agent: &str,
    kind: AgentSessionRefKind,
    value: &str,
) -> Option<PersistedAgentSession> {
    let registry = crate::agents::registry();
    let (_, session) = registry.session_profile_for_exact_report_pair(source, agent)?;
    if !session_profile_accepts_kind(session, kind) {
        return None;
    }
    let session_ref = match kind {
        AgentSessionRefKind::Id => AgentSessionRef::id(value)?,
        AgentSessionRefKind::Path => AgentSessionRef::path(value)?,
    };
    Some(PersistedAgentSession {
        source: source.to_string(),
        agent: agent.to_string(),
        session_ref,
    })
}

#[cfg(test)]
pub fn plan(source: &str, agent: &str, session_ref: &AgentSessionRef) -> Option<AgentResumePlan> {
    let registry = crate::agents::registry();
    let (profile, session) = registry.session_profile_for_exact_report_pair(source, agent)?;
    if !session_profile_accepts_kind(session, session_ref.kind) {
        return None;
    }
    let argv = session.argv(profile.launch().executable(), &session_ref.value);

    Some(AgentResumePlan {
        agent: agent.to_string(),
        argv,
        dedupe_key: dedupe_key(source, agent, session_ref),
        strict_input_readiness: crate::detect::manifest::requires_screen_visible_idle(
            &registry,
            profile.legacy_agent(),
        ),
    })
}

pub fn dedupe_key(source: &str, agent: &str, session_ref: &AgentSessionRef) -> String {
    format!(
        "{source}\u{0}{agent}\u{0}{:?}\u{0}{}",
        session_ref.kind, session_ref.value
    )
}

pub(crate) fn is_official_agent_source(source: &str, agent: &str) -> bool {
    crate::agents::bundled_report_pair(source, agent)
}

fn valid_session_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_SESSION_ID_LEN && !value.chars().any(char::is_control)
}

fn valid_session_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_SESSION_PATH_LEN
        && !value.chars().any(char::is_control)
        && Path::new(value).is_absolute()
}

#[cfg(test)]
pub(crate) fn test_registry(
    id: &str,
    executable: &str,
    strategy: &str,
    token: &str,
) -> std::sync::Arc<crate::agents::RegistrySnapshot> {
    crate::agents::store::snapshot_for_test(vec![
        (format!("agents/{id}/agent.toml"), format!("schema = 1\nid = '{id}'\nname = '{id}'\naliases = ['novel alias']\nstartable = true\n[launch]\nunix = '{executable}'\nwindows = '{executable}'\n")),
        (format!("agents/{id}/resume.toml"), format!("accepted_references = ['id']\npreferred_reference = 'id'\nstrategy = '{strategy}'\ntoken = '{token}'\n")),
    ], 17).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arbitrary_ids_capture_only_closed_explicit_launch_recipes() {
        for (strategy, token, args) in [
            (
                "separate_flag",
                "--session",
                vec!["--session".into(), "abc; data".into()],
            ),
            (
                "subcommand",
                "continue",
                vec!["continue".into(), "abc; data".into()],
            ),
            (
                "joined_flag",
                "--thread=",
                vec!["--thread=abc; data".into()],
            ),
        ] {
            let registry = test_registry("novel-42", "shared-cli", strategy, token);
            let profile = registry.profile_by_id("novel-42").unwrap();
            let captured = persisted_session_from_profile_launch_args(profile, &args).unwrap();
            assert_eq!(captured.source, "herdr:launch");
            assert_eq!(captured.agent, "novel-42");
            assert_eq!(captured.session_ref.value, "abc; data");
            let recipe = PinnedAgentResumeRecipe::capture(profile).unwrap();
            let plan = pinned_plan(&registry, &captured, Some(&recipe)).unwrap();
            assert_eq!(plan.argv[0], "shared-cli");
            assert_eq!(&plan.argv[1..], args);
            assert!(pinned_plan(&registry, &captured, None).is_err());
            assert!(registry
                .profile_for_exact_report_pair("herdr:launch", "novel-42")
                .is_none());
            assert!(registry
                .profile_for_exact_report_pair("herdr:novel-42", "novel-42")
                .is_none());
            let mut extra = args.clone();
            extra.push("--last".into());
            assert!(persisted_session_from_profile_launch_args(profile, &extra).is_none());
        }
    }

    #[test]
    fn pinned_resume_rejects_recipe_changes_missing_packages_and_unpinned_downgrade() {
        let original = test_registry("novel-42", "shared-cli", "separate_flag", "--session");
        let profile = original.profile_by_id("novel-42").unwrap();
        let session =
            persisted_session_from_profile_launch_args(profile, &["--session".into(), "id".into()])
                .unwrap();
        let recipe = PinnedAgentResumeRecipe::capture(profile).unwrap();
        for changed in [
            test_registry("novel-42", "changed-cli", "separate_flag", "--session"),
            test_registry("novel-42", "shared-cli", "separate_flag", "--resume"),
            test_registry("novel-42", "shared-cli", "subcommand", "resume"),
            test_registry("other-package", "shared-cli", "separate_flag", "--session"),
        ] {
            assert!(pinned_plan(&changed, &session, Some(&recipe)).is_err());
        }
        let mut changed = recipe.clone();
        changed.accepted_references.push(AgentSessionRefKind::Path);
        assert!(pinned_plan(&original, &session, Some(&changed)).is_err());
        assert_eq!(
            retained_snapshot_session("herdr:launch", "novel-42", AgentSessionRefKind::Id, "id"),
            Some(session)
        );
        let builtin = PersistedAgentSession {
            source: "herdr:codex".into(),
            agent: "codex".into(),
            session_ref: AgentSessionRef::id("id").unwrap(),
        };
        assert!(pinned_plan(&crate::agents::registry(), &builtin, None).is_ok());
        let changed = test_registry("codex", "codex", "separate_flag", "--session");
        assert!(pinned_plan(&changed, &builtin, None).is_err());
    }

    fn absolute_test_path(name: &str) -> String {
        std::env::current_dir()
            .unwrap()
            .join(name)
            .display()
            .to_string()
    }

    const OFFICIAL_SESSION_PAIRS: [(&str, &str); 17] = [
        ("herdr:pi", "pi"),
        ("herdr:claude", "claude"),
        ("herdr:codex", "codex"),
        ("herdr:cursor", "cursor"),
        ("herdr:devin", "devin"),
        ("herdr:antigravity_cli", "agy"),
        ("herdr:omp", "omp"),
        ("herdr:mastracode", "mastracode"),
        ("herdr:opencode", "opencode"),
        ("herdr:copilot", "copilot"),
        ("herdr:kimi", "kimi"),
        ("herdr:droid", "droid"),
        ("herdr:grok", "grok"),
        ("herdr:hermes", "hermes"),
        ("herdr:kilo", "kilo"),
        ("herdr:qodercli", "qodercli"),
        ("herdr:qwen", "qwen"),
    ];

    #[test]
    fn official_source_identity_requires_every_exact_source_and_canonical_pair() {
        for (index, (source, agent)) in OFFICIAL_SESSION_PAIRS.into_iter().enumerate() {
            assert!(is_official_agent_source(source, agent), "{source} {agent}");
            assert!(!is_official_agent_source("custom:agent", agent));

            let other_agent = OFFICIAL_SESSION_PAIRS[(index + 1) % OFFICIAL_SESSION_PAIRS.len()].1;
            assert!(!is_official_agent_source(source, other_agent));
        }

        for (source, alias) in [
            ("herdr:claude", "claude-code"),
            ("herdr:cursor", "cursor-agent"),
            ("herdr:devin", "devin-cli"),
            ("herdr:antigravity_cli", "antigravity"),
            ("herdr:mastracode", "mastra-code"),
            ("herdr:opencode", "open-code"),
            ("herdr:copilot", "github-copilot"),
            ("herdr:kimi", "kimi-code"),
            ("herdr:grok", "grok-build"),
            ("herdr:hermes", "hermes-agent"),
            ("herdr:kilo", "kilo-code"),
            ("herdr:qodercli", "qoder"),
            ("herdr:qwen", "qwen-code"),
        ] {
            assert!(!is_official_agent_source(source, alias), "{source} {alias}");
        }

        for agent in ["gemini", "cline", "kiro", "amp", "maki"] {
            assert!(!is_official_agent_source("herdr:custom", agent));
        }
    }

    #[test]
    fn codex_noncanonical_resume_launch_has_no_explicit_session() {
        assert_eq!(
            persisted_session_from_launch_args(
                crate::detect::Agent::Codex,
                &["resume".into(), "codex-session".into()]
            )
            .unwrap()
            .session_ref
            .value,
            "codex-session"
        );
        assert!(persisted_session_from_launch_args(
            crate::detect::Agent::Codex,
            &["resume".into(), "--last".into()]
        )
        .is_none());
        assert!(persisted_session_from_launch_args(
            crate::detect::Agent::Codex,
            &["resume".into(), "not-a-session".into(), "--last".into()]
        )
        .is_none());
        assert!(persisted_session_from_launch_args(
            crate::detect::Agent::Codex,
            &[
                "--remote".into(),
                "ws://example.test".into(),
                "resume".into(),
                "remote-session".into(),
            ]
        )
        .is_none());
    }

    #[test]
    fn planner_allows_supported_agents() {
        let pi_session = absolute_test_path("pi-session.jsonl");
        let omp_session = absolute_test_path("omp-session.jsonl");
        assert_eq!(
            plan(
                "herdr:claude",
                "claude",
                &AgentSessionRef::id("claude-session").unwrap()
            )
            .unwrap()
            .argv,
            vec!["claude", "--resume", "claude-session"]
        );
        assert_eq!(
            plan(
                "herdr:codex",
                "codex",
                &AgentSessionRef::id("codex-session").unwrap()
            )
            .unwrap()
            .argv,
            vec!["codex", "resume", "codex-session"]
        );
        assert_eq!(
            plan(
                "herdr:copilot",
                "copilot",
                &AgentSessionRef::id("copilot-session").unwrap()
            )
            .unwrap()
            .argv,
            vec!["copilot", "--resume=copilot-session"]
        );
        assert_eq!(
            plan(
                "herdr:devin",
                "devin",
                &AgentSessionRef::id("devin-session").unwrap()
            )
            .unwrap()
            .argv,
            vec!["devin", "--resume", "devin-session"]
        );
        assert_eq!(
            plan(
                "herdr:droid",
                "droid",
                &AgentSessionRef::id("droid-session").unwrap()
            )
            .unwrap()
            .argv,
            vec!["droid", "--resume", "droid-session"]
        );
        assert_eq!(
            plan(
                "herdr:kimi",
                "kimi",
                &AgentSessionRef::id("kimi-session").unwrap()
            )
            .unwrap()
            .argv,
            vec!["kimi", "--session", "kimi-session"]
        );
        assert_eq!(
            plan(
                "herdr:mastracode",
                "mastracode",
                &AgentSessionRef::id("mastracode-session").unwrap()
            )
            .unwrap()
            .argv,
            vec!["mastracode", "--thread", "mastracode-session"]
        );
        assert_eq!(
            plan(
                "herdr:pi",
                "pi",
                &AgentSessionRef::path(&pi_session).unwrap()
            )
            .unwrap()
            .argv,
            vec!["pi", "--session", pi_session.as_str()]
        );
        assert_eq!(
            plan(
                "herdr:pi",
                "pi",
                &AgentSessionRef::id("pi-session-id").unwrap()
            )
            .unwrap()
            .argv,
            vec!["pi", "--session", "pi-session-id"]
        );
        assert_eq!(
            plan(
                "herdr:omp",
                "omp",
                &AgentSessionRef::path(&omp_session).unwrap()
            )
            .unwrap()
            .argv,
            vec!["omp", format!("--resume={omp_session}").as_str()]
        );
        assert_eq!(
            plan(
                "herdr:omp",
                "omp",
                &AgentSessionRef::id("omp-session-id").unwrap()
            )
            .unwrap()
            .argv,
            vec!["omp", "--resume=omp-session-id"]
        );
        assert_eq!(
            plan(
                "herdr:hermes",
                "hermes",
                &AgentSessionRef::id("hermes-session").unwrap()
            )
            .unwrap()
            .argv,
            vec!["hermes", "--resume", "hermes-session"]
        );
        assert_eq!(
            plan(
                "herdr:opencode",
                "opencode",
                &AgentSessionRef::id("opencode-session").unwrap()
            )
            .unwrap()
            .argv,
            vec!["opencode", "--session", "opencode-session"]
        );
        assert_eq!(
            plan(
                "herdr:qodercli",
                "qodercli",
                &AgentSessionRef::id("qoder-session").unwrap()
            )
            .unwrap()
            .argv,
            vec!["qodercli", "--resume", "qoder-session"]
        );
        assert_eq!(
            plan(
                "herdr:qwen",
                "qwen",
                &AgentSessionRef::id("qwen-session").unwrap()
            )
            .unwrap()
            .argv,
            vec!["qwen", "--resume", "qwen-session"]
        );
        assert_eq!(
            plan(
                "herdr:kilo",
                "kilo",
                &AgentSessionRef::id("kilo-session").unwrap()
            )
            .unwrap()
            .argv,
            vec!["kilo", "--session", "kilo-session"]
        );
        assert_eq!(
            plan(
                "herdr:cursor",
                "cursor",
                &AgentSessionRef::id("cursor-session").unwrap()
            )
            .unwrap()
            .argv,
            vec![
                if cfg!(windows) {
                    "cursor-agent.cmd"
                } else {
                    "cursor-agent"
                },
                "--resume",
                "cursor-session",
            ]
        );
        assert_eq!(
            plan(
                "herdr:antigravity_cli",
                "agy",
                &AgentSessionRef::id("agy-session").unwrap()
            )
            .unwrap()
            .argv,
            vec!["agy", "--conversation", "agy-session"]
        );
        assert_eq!(
            plan(
                "herdr:grok",
                "grok",
                &AgentSessionRef::id("grok-session").unwrap()
            )
            .unwrap()
            .argv,
            vec!["grok", "--resume", "grok-session"]
        );
    }

    #[test]
    fn planner_rejects_custom_and_unsupported_path_refs() {
        let claude_session = absolute_test_path("claude-session");
        assert!(plan(
            "custom:claude",
            "claude",
            &AgentSessionRef::id("session").unwrap()
        )
        .is_none());
        assert!(plan(
            "herdr:claude",
            "claude",
            &AgentSessionRef::path(&claude_session).unwrap()
        )
        .is_none());
    }

    #[test]
    fn report_reference_validation_rejects_malformed_values_and_custom_sources() {
        assert!(session_ref_from_report("herdr:pi", "pi", Some("bad\nid".into()), None).is_none());
        assert!(
            session_ref_from_report("herdr:pi", "pi", None, Some("relative.jsonl".into()))
                .is_none()
        );
        assert!(session_ref_from_report("custom:pi", "pi", Some("pi-id".into()), None).is_none());
    }

    #[test]
    fn report_reference_policy_is_id_only_except_for_pi_and_omp_path_preference() {
        let absolute_path = absolute_test_path("reported-session.jsonl");

        for (source, agent) in OFFICIAL_SESSION_PAIRS {
            let selected = session_ref_from_report(
                source,
                agent,
                Some(format!("{agent}-id")),
                Some(absolute_path.clone()),
            )
            .unwrap();
            let path_preferred = matches!(agent, "pi" | "omp");
            assert_eq!(
                selected.kind,
                if path_preferred {
                    AgentSessionRefKind::Path
                } else {
                    AgentSessionRefKind::Id
                },
                "{source} {agent}"
            );
            let expected_value = if path_preferred {
                absolute_path.clone()
            } else {
                format!("{agent}-id")
            };
            assert_eq!(selected.value, expected_value);
            if !path_preferred {
                assert!(
                    session_ref_from_report(source, agent, None, Some(absolute_path.clone()))
                        .is_none()
                );
            }
        }

        for (source, agent) in [("herdr:pi", "pi"), ("herdr:omp", "omp")] {
            let fallback = session_ref_from_report(
                source,
                agent,
                Some(format!("{agent}-id")),
                Some("relative-session.jsonl".into()),
            )
            .unwrap();
            assert_eq!(fallback.kind, AgentSessionRefKind::Id);
            assert_eq!(fallback.value, format!("{agent}-id"));
        }
    }

    #[test]
    fn snapshot_reference_kinds_match_the_exact_session_capability_matrix() {
        let absolute_path = absolute_test_path("snapshot-session.jsonl");

        for (source, agent) in OFFICIAL_SESSION_PAIRS {
            assert!(session_ref_from_snapshot(
                source,
                agent,
                AgentSessionRefKind::Id,
                "session-id"
            )
            .is_some());
            assert_eq!(
                session_ref_from_snapshot(source, agent, AgentSessionRefKind::Path, &absolute_path)
                    .is_some(),
                matches!(agent, "pi" | "omp"),
                "{source} {agent}"
            );
        }

        assert!(session_ref_from_snapshot(
            "custom:pi",
            "pi",
            AgentSessionRefKind::Id,
            "session-id"
        )
        .is_none());
        assert!(session_ref_from_snapshot(
            "herdr:qwen",
            "qwen-code",
            AgentSessionRefKind::Id,
            "session-id"
        )
        .is_none());
    }

    #[test]
    fn normalize_session_start_source_allows_known_values() {
        for source in [
            "startup", "resume", "clear", "compact", "branch", "new", "fork", "select",
        ] {
            assert_eq!(
                normalize_session_start_source(Some(source.into())),
                Some(source.into())
            );
        }
        assert_eq!(
            normalize_session_start_source(Some(" resume ".into())),
            Some("resume".into())
        );
        assert_eq!(normalize_session_start_source(Some("other".into())), None);
        assert_eq!(normalize_session_start_source(None), None);
    }

    #[test]
    fn ids_are_data_not_shell_text() {
        let id = "abc; rm -rf /";
        let codex_plan = plan("herdr:codex", "codex", &AgentSessionRef::id(id).unwrap()).unwrap();
        assert_eq!(codex_plan.argv, vec!["codex", "resume", id]);

        let copilot_plan = plan(
            "herdr:copilot",
            "copilot",
            &AgentSessionRef::id(id).unwrap(),
        )
        .unwrap();
        assert_eq!(copilot_plan.argv, vec!["copilot", "--resume=abc; rm -rf /"]);

        let devin_plan = plan("herdr:devin", "devin", &AgentSessionRef::id(id).unwrap()).unwrap();
        assert_eq!(devin_plan.argv, vec!["devin", "--resume", id]);
    }

    #[test]
    fn planner_rejects_path_refs_for_every_id_only_agent() {
        let absolute_path = absolute_test_path("id-only-session");
        let session_ref = AgentSessionRef::path(absolute_path).unwrap();

        for (source, agent) in OFFICIAL_SESSION_PAIRS {
            if matches!(agent, "pi" | "omp") {
                continue;
            }
            assert!(
                plan(source, agent, &session_ref).is_none(),
                "{source} {agent}"
            );
        }
    }
}
