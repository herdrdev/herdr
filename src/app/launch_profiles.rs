//! Named launch lanes offered by the home composer.
//!
//! A profile is one way to start one agent. The built-in lanes are an agent's
//! own executable with the flags Herdr builds from the probed catalog. A
//! configured lane may carry its own command and environment instead, because
//! a provider is not always a binary: a Kimi session is Claude Code pointed at
//! a different base URL through a dozen environment variables, and a Codex
//! account is a `CODEX_HOME` directory. Naming the launcher the operator
//! already maintains beats restating its environment here, where it would
//! drift from whatever that launcher does next.

use crate::config::LaunchProfileConfig;
use crate::detect::{parse_agent_label, Agent};

/// Which quota window a lane reports.
///
/// Separate from `UsageProvider`, which names the providers whose historical
/// token logs Herdr scans. Kimi and Antigravity publish quotas but keep no local
/// log, so the two sets are not the same and one enum cannot serve both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QuotaSource {
    Claude,
    Codex,
    Kimi,
    Agy,
}

impl QuotaSource {
    /// This lane's window in a collected snapshot.
    pub(crate) fn usage(
        self,
        snapshot: &crate::provider_usage::ProviderUsageSnapshot,
    ) -> &crate::provider_usage::AccountUsage {
        let provider = match self {
            Self::Claude => crate::provider_usage::QuotaProvider::Claude,
            Self::Codex => crate::provider_usage::QuotaProvider::Codex,
            Self::Kimi => crate::provider_usage::QuotaProvider::Kimi,
            Self::Agy => crate::provider_usage::QuotaProvider::Agy,
        };
        snapshot.primary_usage(provider)
    }

    fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "claude" | "claude_code" | "claude-code" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "kimi" => Some(Self::Kimi),
            "agy" | "antigravity" => Some(Self::Agy),
            _ => None,
        }
    }
}

/// A resolved lane: valid agent, expanded environment, ready to dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LaunchProfile {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) agent: Agent,
    /// Empty when Herdr builds the argv from the catalog.
    pub(crate) command: Vec<String>,
    pub(crate) env: Vec<(String, String)>,
    pub(crate) quota: Option<QuotaSource>,
}

impl LaunchProfile {
    /// True when this lane supplies its own model, effort and permission flags.
    ///
    /// The composer hides those pickers for such a lane: it cannot know what
    /// `--model` the command would accept, and appending Herdr's own flags to
    /// a launcher that already sets them is how a session fails to start.
    pub(crate) fn owns_its_flags(&self) -> bool {
        !self.command.is_empty()
    }

    /// The built-in lane for an agent: its executable, Herdr's flags, no
    /// environment of its own.
    pub(crate) fn builtin(agent: Agent) -> Self {
        Self {
            id: crate::detect::agent_label(agent).to_ascii_lowercase(),
            label: crate::detect::agent_label(agent).to_string(),
            agent,
            command: Vec::new(),
            env: Vec::new(),
            quota: match agent {
                Agent::Claude => Some(QuotaSource::Claude),
                Agent::Codex => Some(QuotaSource::Codex),
                Agent::Kimi => Some(QuotaSource::Kimi),
                _ => None,
            },
        }
    }
}

/// Expands a leading `~/` against the home directory. Anything else is passed
/// through: an environment value is not a path in general.
fn expand_home(value: &str) -> String {
    let Some(rest) = value.strip_prefix("~/") else {
        return value.to_string();
    };
    match std::env::var_os("HOME").filter(|home| !home.is_empty()) {
        Some(home) => std::path::Path::new(&home)
            .join(rest)
            .to_string_lossy()
            .into_owned(),
        None => value.to_string(),
    }
}

/// The lanes the composer offers: one built-in per dispatchable agent, then
/// every configured lane that names an agent Herdr recognises.
///
/// An entry Herdr cannot resolve is dropped with a log line rather than
/// failing the load: a typo in one profile must not cost the operator the
/// composer. A configured lane may share an id with a built-in, in which case
/// it replaces it — that is how an operator repoints `codex` at a profile
/// directory without gaining a duplicate row.
pub(crate) fn resolve(configured: &[LaunchProfileConfig]) -> Vec<LaunchProfile> {
    let mut profiles: Vec<LaunchProfile> = crate::app::home::dispatchable_agents()
        .iter()
        .map(|agent| LaunchProfile::builtin(*agent))
        .collect();

    for entry in configured {
        let id = entry.id.trim();
        if id.is_empty() {
            tracing::warn!("launch profile without an id ignored");
            continue;
        }
        let Some(agent) = parse_agent_label(&entry.agent) else {
            tracing::warn!(
                profile = id,
                agent = entry.agent,
                "launch profile names an unknown agent, ignored"
            );
            continue;
        };
        if entry.usage.as_deref().is_some_and(|usage| {
            let unknown = QuotaSource::parse(usage).is_none();
            if unknown {
                tracing::warn!(profile = id, usage, "launch profile names an unknown quota");
            }
            unknown
        }) {
            // The lane is still usable; it just reports no usage.
        }
        let label = if entry.label.trim().is_empty() {
            id.to_string()
        } else {
            entry.label.trim().to_string()
        };
        let profile = LaunchProfile {
            id: id.to_string(),
            label,
            agent,
            command: entry
                .command
                .iter()
                .map(|argument| expand_home(argument))
                .collect(),
            env: entry
                .env
                .iter()
                .map(|(name, value)| (name.clone(), expand_home(value)))
                .collect(),
            quota: entry
                .usage
                .as_deref()
                .and_then(QuotaSource::parse)
                .or_else(|| LaunchProfile::builtin(agent).quota),
        };
        match profiles.iter_mut().find(|known| known.id == profile.id) {
            Some(known) => *known = profile,
            None => profiles.push(profile),
        }
    }

    profiles
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(id: &str, agent: &str) -> LaunchProfileConfig {
        LaunchProfileConfig {
            id: id.into(),
            agent: agent.into(),
            ..Default::default()
        }
    }

    #[test]
    fn the_builtin_lanes_are_the_dispatchable_agents() {
        let profiles = resolve(&[]);
        assert_eq!(
            profiles
                .iter()
                .map(|profile| profile.id.as_str())
                .collect::<Vec<_>>(),
            ["claude", "codex"]
        );
        assert!(
            profiles.iter().all(|profile| !profile.owns_its_flags()),
            "a built-in lane takes the flags Herdr builds"
        );
    }

    #[test]
    fn a_lane_with_a_command_owns_its_flags() {
        let mut kimi = config("kimi", "claude");
        kimi.label = "Kimi K3".into();
        kimi.command = vec!["bash".into(), "-lc".into(), "cck3".into()];
        kimi.usage = Some("kimi".into());

        let profiles = resolve(&[kimi]);
        let lane = profiles.last().expect("configured lane");

        assert_eq!(lane.id, "kimi");
        assert_eq!(lane.label, "Kimi K3");
        assert_eq!(lane.agent, Agent::Claude, "the lane still runs Claude Code");
        assert!(lane.owns_its_flags());
        assert_eq!(lane.quota, Some(QuotaSource::Kimi));
    }

    #[test]
    fn antigravity_quota_names_resolve_to_its_usage_snapshot() {
        let mut profile = config("agy", "claude");
        profile.usage = Some("antigravity".into());
        let lane = resolve(&[profile]).pop().expect("configured lane");

        assert_eq!(lane.quota, Some(QuotaSource::Agy));
        assert!(lane.quota.unwrap().usage(&Default::default()).is_empty());
        assert_eq!(QuotaSource::parse("agy"), Some(QuotaSource::Agy));
    }

    #[test]
    fn an_env_only_lane_keeps_herdrs_flag_building() {
        let mut profile = config("codex-scalable", "codex");
        profile
            .env
            .insert("CODEX_HOME".into(), "~/.codex-profiles/scalable".into());

        let profiles = resolve(&[profile]);
        let lane = profiles.last().expect("configured lane");

        assert!(
            !lane.owns_its_flags(),
            "no command, so the model and effort pickers still apply"
        );
        assert_eq!(lane.env.len(), 1);
        assert!(
            !lane.env[0].1.starts_with('~'),
            "the home directory is expanded before spawn: {:?}",
            lane.env[0]
        );
        assert_eq!(
            lane.quota,
            Some(QuotaSource::Codex),
            "an unnamed quota falls back to the agent's own"
        );
    }

    #[test]
    fn an_unresolvable_lane_is_dropped_rather_than_failing_the_load() {
        let profiles = resolve(&[
            config("", "claude"),
            config("nonsense", "not-an-agent"),
            config("fine", "codex"),
        ]);
        assert_eq!(
            profiles
                .iter()
                .map(|profile| profile.id.as_str())
                .collect::<Vec<_>>(),
            ["claude", "codex", "fine"]
        );
    }

    #[test]
    fn a_configured_lane_replaces_the_builtin_sharing_its_id() {
        let mut profile = config("codex", "codex");
        profile.env.insert("CODEX_HOME".into(), "/elsewhere".into());

        let profiles = resolve(&[profile]);

        assert_eq!(profiles.len(), 2, "no duplicate row: {profiles:?}");
        let codex = profiles
            .iter()
            .find(|profile| profile.id == "codex")
            .expect("codex lane");
        assert_eq!(
            codex.env,
            [("CODEX_HOME".to_string(), "/elsewhere".to_string())]
        );
    }
}
