use std::{collections::BTreeMap, fmt, path::PathBuf};

use serde::{
    de::{IgnoredAny, MapAccess, Visitor},
    Deserialize, Deserializer,
};

use crate::{agents::presentation::SoundDefaultPolicy, detect::Agent};

use super::io::resolve_config_relative_path;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct SoundConfig {
    pub enabled: bool,
    /// Optional mp3 file path used for all notification sounds.
    /// Relative paths are resolved from the config file's directory.
    pub path: Option<PathBuf>,
    /// Optional mp3 file path for "done" notifications.
    /// Relative paths are resolved from the config file's directory.
    pub done_path: Option<PathBuf>,
    /// Optional mp3 file path for "request" notifications.
    /// Relative paths are resolved from the config file's directory.
    pub request_path: Option<PathBuf>,
    pub agents: AgentSoundOverrides,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentSoundOverrides {
    overrides: BTreeMap<String, AgentSoundSetting>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentSoundSetting {
    #[default]
    Default,
    On,
    Off,
}

#[cfg(test)]
fn setting_for_policy(policy: SoundDefaultPolicy) -> AgentSoundSetting {
    match policy {
        SoundDefaultPolicy::Default => AgentSoundSetting::Default,
        SoundDefaultPolicy::Off => AgentSoundSetting::Off,
    }
}

impl SoundConfig {
    pub fn allows(&self, agent: Option<Agent>) -> bool {
        if !self.enabled {
            return false;
        }

        !matches!(self.agents.for_agent(agent), AgentSoundSetting::Off)
    }

    /// Apply local user policy to package metadata resolved by the notifying
    /// server. A missing key is authoritative, not a request for local lookup.
    pub(crate) fn allows_resolved_sound(&self, key: Option<&str>, default_off: bool) -> bool {
        self.enabled
            && !matches!(
                self.agents.for_resolved_sound(key, default_off),
                AgentSoundSetting::Off
            )
    }

    pub fn path_for(&self, sound: crate::sound::Sound) -> Option<PathBuf> {
        let path = match sound {
            crate::sound::Sound::Done => self.done_path.as_ref().or(self.path.as_ref()),
            crate::sound::Sound::Request => self.request_path.as_ref().or(self.path.as_ref()),
        }?;

        Some(resolve_config_relative_path(path))
    }

    pub fn diagnostics(&self) -> Vec<String> {
        let mut diagnostics = Vec::new();
        for (field, path) in [
            ("ui.sound.path", self.path.as_ref()),
            ("ui.sound.done_path", self.done_path.as_ref()),
            ("ui.sound.request_path", self.request_path.as_ref()),
        ] {
            let Some(path) = path else {
                continue;
            };

            let resolved = resolve_config_relative_path(path);
            if resolved
                .extension()
                .and_then(|ext| ext.to_str())
                .is_none_or(|ext: &str| !ext.eq_ignore_ascii_case("mp3"))
            {
                diagnostics.push(format!(
                    "unsupported sound file format: {field} = {} resolves to {}; expected an mp3 file; using default sound",
                    path.display(),
                    resolved.display()
                ));
                continue;
            }

            if !resolved.exists() {
                diagnostics.push(format!(
                    "missing sound file: {field} = {} resolves to {}; using default sound",
                    path.display(),
                    resolved.display()
                ));
            } else if !resolved.is_file() {
                diagnostics.push(format!(
                    "invalid sound file: {field} = {} resolves to {}; using default sound",
                    path.display(),
                    resolved.display()
                ));
            }
        }
        diagnostics
    }
}

impl AgentSoundOverrides {
    fn for_resolved_sound(&self, key: Option<&str>, default_off: bool) -> AgentSoundSetting {
        key.and_then(|key| self.overrides.get(key))
            .copied()
            .unwrap_or({
                if default_off {
                    AgentSoundSetting::Off
                } else {
                    AgentSoundSetting::Default
                }
            })
    }

    pub fn for_agent(&self, agent: Option<Agent>) -> AgentSoundSetting {
        let Some(agent) = agent else {
            return AgentSoundSetting::Default;
        };
        let registry = crate::agents::registry();
        let Some(sound) = registry
            .profile_by_agent(agent)
            .and_then(|profile| profile.sound())
        else {
            return AgentSoundSetting::Default;
        };

        self.for_resolved_sound(
            Some(sound.config_key()),
            sound.default_policy() == SoundDefaultPolicy::Off,
        )
    }
}

impl<'de> Deserialize<'de> for AgentSoundOverrides {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct OverridesVisitor;

        impl<'de> Visitor<'de> for OverridesVisitor {
            type Value = AgentSoundOverrides;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an agent sound override table")
            }

            fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                let mut result = AgentSoundOverrides::default();
                let registry = crate::agents::registry();

                #[derive(Deserialize)]
                #[serde(untagged)]
                enum RawSetting {
                    Setting(AgentSoundSetting),
                    Other(IgnoredAny),
                }

                while let Some(key) = map.next_key::<String>()? {
                    // Same bounded, exact namespace as package sound keys.
                    let valid_key = !key.is_empty()
                        && key.len() <= 128
                        && key.bytes().all(|byte| {
                            byte.is_ascii_lowercase()
                                || byte.is_ascii_digit()
                                || b"-_".contains(&byte)
                        })
                        && key.bytes().any(|byte| byte.is_ascii_lowercase());
                    if !valid_key {
                        map.next_value::<IgnoredAny>()?;
                        continue;
                    }
                    let setting = match map.next_value::<RawSetting>()? {
                        RawSetting::Setting(setting) => setting,
                        RawSetting::Other(_) => {
                            if registry.sound_profile_by_config_key(&key).is_some() {
                                return Err(serde::de::Error::custom(format!(
                                    "invalid sound setting for `{key}`; expected default, on, or off"
                                )));
                            }
                            continue;
                        }
                    };
                    // Preserve explicit choices even if they match today's package
                    // default; a later server notification can carry a new default.
                    if result.overrides.insert(key.clone(), setting).is_some() {
                        return Err(serde::de::Error::custom(format!("duplicate field `{key}`")));
                    }
                }

                Ok(result)
            }
        }

        deserializer.deserialize_map(OverridesVisitor)
    }
}

impl Default for SoundConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            path: None,
            done_path: None,
            request_path: None,
            agents: AgentSoundOverrides::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::config::{config_path, Config};

    const EXPECTED_SOUND_PROFILES: [(Agent, Option<&str>, AgentSoundSetting); 23] = [
        (Agent::Pi, Some("pi"), AgentSoundSetting::Default),
        (Agent::Claude, Some("claude"), AgentSoundSetting::Default),
        (Agent::Codex, Some("codex"), AgentSoundSetting::Default),
        (Agent::Gemini, Some("gemini"), AgentSoundSetting::Default),
        (Agent::Cursor, Some("cursor"), AgentSoundSetting::Default),
        (Agent::Devin, Some("devin"), AgentSoundSetting::Default),
        (Agent::Antigravity, Some("agy"), AgentSoundSetting::Default),
        (Agent::Cline, Some("cline"), AgentSoundSetting::Default),
        (Agent::Omp, None, AgentSoundSetting::Default),
        (Agent::Mastracode, None, AgentSoundSetting::Default),
        (
            Agent::OpenCode,
            Some("open_code"),
            AgentSoundSetting::Default,
        ),
        (
            Agent::GithubCopilot,
            Some("github_copilot"),
            AgentSoundSetting::Default,
        ),
        (Agent::Kimi, Some("kimi"), AgentSoundSetting::Default),
        (Agent::Kiro, Some("kiro"), AgentSoundSetting::Default),
        (Agent::Droid, Some("droid"), AgentSoundSetting::Off),
        (Agent::Amp, Some("amp"), AgentSoundSetting::Default),
        (Agent::Grok, Some("grok"), AgentSoundSetting::Default),
        (Agent::Hermes, Some("hermes"), AgentSoundSetting::Default),
        (Agent::Kilo, Some("kilo"), AgentSoundSetting::Default),
        (
            Agent::Qodercli,
            Some("qodercli"),
            AgentSoundSetting::Default,
        ),
        (Agent::Qwen, Some("qwen"), AgentSoundSetting::Default),
        (Agent::Maki, Some("maki"), AgentSoundSetting::Default),
        (Agent::Muse, Some("muse"), AgentSoundSetting::Default),
    ];

    fn config_with_all_sound_keys(setting: &str) -> Config {
        let entries = EXPECTED_SOUND_PROFILES
            .iter()
            .filter_map(|(_, key, _)| key.map(|key| format!("{key} = \"{setting}\"")))
            .collect::<Vec<_>>()
            .join("\n");
        toml::from_str(&format!("[ui.sound.agents]\n{entries}\n")).unwrap()
    }

    #[test]
    fn registry_preserves_the_existing_sound_key_and_default_matrix() {
        let registry = crate::agents::registry();
        assert_eq!(EXPECTED_SOUND_PROFILES.len(), 23);

        for (agent, key, default) in EXPECTED_SOUND_PROFILES {
            let profile = registry.profile_for_agent(agent);
            assert_eq!(profile.sound().map(|sound| sound.config_key()), key);
            assert_eq!(
                AgentSoundOverrides::default().for_agent(Some(agent)),
                default
            );

            if let Some(key) = key {
                let sound = registry
                    .sound_profile_by_config_key(key)
                    .expect("registered sound config key");
                assert!(profile
                    .sound()
                    .is_some_and(|registered| std::ptr::eq(registered, sound)));
                assert_eq!(setting_for_policy(sound.default_policy()), default);
            }
        }

        assert_eq!(
            registry
                .known_profiles()
                .filter(|profile| profile.sound().is_some())
                .count(),
            21
        );
        assert!(registry.sound_profile_by_config_key("unknown").is_none());
        assert_eq!(
            AgentSoundOverrides::default().for_agent(None),
            AgentSoundSetting::Default
        );
        assert_eq!(
            AgentSoundOverrides::default().for_agent(Some(Agent::parse("future-agent").unwrap())),
            AgentSoundSetting::Default
        );
    }

    #[test]
    fn explicit_registry_defaults_remain_local_choices_after_package_changes() {
        let config: Config = toml::from_str(
            r#"
[ui.sound.agents]
claude = "default"
droid = "off"
"#,
        )
        .unwrap();

        assert!(config.ui.sound.allows_resolved_sound(Some("claude"), true));
        assert!(!config.ui.sound.allows_resolved_sound(Some("droid"), false));
    }

    #[test]
    fn rejects_duplicate_known_key_even_when_the_value_matches_package_default() {
        type ValueDeserializer<'a> = serde::de::value::StrDeserializer<'a, serde::de::value::Error>;
        let entries = [
            (
                ValueDeserializer::new("claude"),
                ValueDeserializer::new("default"),
            ),
            (
                ValueDeserializer::new("claude"),
                ValueDeserializer::new("default"),
            ),
        ];
        let result: Result<AgentSoundOverrides, serde::de::value::Error> =
            <AgentSoundOverrides as serde::Deserialize>::deserialize(
                serde::de::value::MapDeserializer::new(entries.into_iter()),
            );

        assert!(result
            .unwrap_err()
            .to_string()
            .contains("duplicate field `claude`"));
    }

    #[test]
    fn all_registered_sound_keys_parse_explicit_on_and_off_overrides() {
        for (setting_name, expected) in [
            ("on", AgentSoundSetting::On),
            ("off", AgentSoundSetting::Off),
        ] {
            let config = config_with_all_sound_keys(setting_name);
            for (agent, key, _) in EXPECTED_SOUND_PROFILES {
                let expected = if key.is_some() {
                    expected
                } else {
                    AgentSoundSetting::Default
                };
                assert_eq!(config.ui.sound.agents.for_agent(Some(agent)), expected);
            }
        }
    }

    #[test]
    fn sound_table_config_parses_without_exposing_typed_agent_fields() {
        let toml = r#"
[ui.sound]
enabled = true
path = "sounds/all.mp3"
done_path = "sounds/done.mp3"
request_path = "/tmp/request.mp3"

[ui.sound.agents]
droid = "on"
claude = "off"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.ui.sound.enabled);
        assert_eq!(config.ui.sound.path, Some(PathBuf::from("sounds/all.mp3")));
        assert_eq!(
            config.ui.sound.done_path,
            Some(PathBuf::from("sounds/done.mp3"))
        );
        assert_eq!(
            config.ui.sound.request_path,
            Some(PathBuf::from("/tmp/request.mp3"))
        );
        assert_eq!(
            config.ui.sound.agents.for_agent(Some(Agent::Droid)),
            AgentSoundSetting::On
        );
        assert_eq!(
            config.ui.sound.agents.for_agent(Some(Agent::Claude)),
            AgentSoundSetting::Off
        );
        assert_eq!(
            config.ui.sound.agents.for_agent(Some(Agent::Pi)),
            AgentSoundSetting::Default
        );
    }

    #[test]
    fn unknown_alias_case_and_whitespace_sound_keys_are_ignored() {
        let config: Config = toml::from_str(
            r#"
[ui.sound.agents]
pi = "on"
omp = "not-a-setting"
mastracode = "on"
opencode = "off"
copilot = "off"
antigravity = "off"
"claude-code" = "off"
Claude = "off"
" claude " = "off"
unknown = "not-a-setting"
"#,
        )
        .expect("unknown sound keys remain ignored");

        assert_eq!(
            config.ui.sound.agents.for_agent(Some(Agent::Pi)),
            AgentSoundSetting::On
        );
        for agent in [
            Agent::Omp,
            Agent::Mastracode,
            Agent::OpenCode,
            Agent::GithubCopilot,
            Agent::Antigravity,
            Agent::Claude,
        ] {
            assert_eq!(
                config.ui.sound.agents.for_agent(Some(agent)),
                AgentSoundSetting::Default
            );
        }
        assert_eq!(
            config.ui.sound.agents.for_agent(Some(Agent::Droid)),
            AgentSoundSetting::Off
        );
    }

    #[test]
    fn remote_sound_keys_are_exact_and_preserved_before_the_package_is_known() {
        let config: SoundConfig = toml::from_str(
            "[agents]\nfuture_key = 'off'\nfuture_on = 'on'\nfuture_default = 'default'\n",
        )
        .unwrap();
        assert!(!config.allows_resolved_sound(Some("future_key"), false));
        assert!(config.allows_resolved_sound(Some("future_on"), true));
        assert!(config.allows_resolved_sound(Some("future_default"), true));
        assert!(config.allows_resolved_sound(Some("future-key"), false));
        assert!(config.allows_resolved_sound(None, false));
        assert!(!config.allows_resolved_sound(Some("unconfigured_key"), true));
        assert!(config.allows_resolved_sound(Some("unconfigured_key"), false));
    }

    #[test]
    fn invalid_known_sound_settings_still_fail_validation() {
        assert!(toml::from_str::<SoundConfig>("[agents]\nclaude = 'invalid'\n").is_err());
        assert!(toml::from_str::<SoundConfig>("[agents]\nunknown = 'invalid'\n").is_ok());
    }

    #[test]
    fn sound_path_resolution_prefers_specific_over_global() {
        let config: Config = toml::from_str(
            r#"
[ui.sound]
path = "sounds/all.mp3"
done_path = "sounds/done.mp3"
"#,
        )
        .unwrap();

        let config_root = config_path().parent().unwrap().to_path_buf();
        assert_eq!(
            config.ui.sound.path_for(crate::sound::Sound::Done),
            Some(config_root.join("sounds/done.mp3"))
        );
        assert_eq!(
            config.ui.sound.path_for(crate::sound::Sound::Request),
            Some(config_root.join("sounds/all.mp3"))
        );
    }

    #[test]
    fn missing_sound_file_produces_diagnostic() {
        let config: Config = toml::from_str(
            r#"
[ui.sound]
done_path = "sounds/missing.mp3"
"#,
        )
        .unwrap();

        let diagnostics = config.collect_diagnostics();
        assert!(diagnostics.iter().any(
            |diag| diag.contains("ui.sound.done_path") && diag.contains("using default sound")
        ));
    }

    #[test]
    fn non_mp3_sound_file_produces_diagnostic() {
        let config: Config = toml::from_str(
            r#"
[ui.sound]
path = "sounds/notification.wav"
"#,
        )
        .unwrap();

        let diagnostics = config.collect_diagnostics();
        assert!(diagnostics.iter().any(|diag| {
            diag.contains("ui.sound.path") && diag.contains("expected an mp3 file")
        }));
    }
}
