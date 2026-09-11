use std::io;

use tracing::{debug, warn};

use crate::protocol::NotifyKind;

use super::shell;

pub(super) fn handle_shell_notification_effects(
    effects: Vec<shell::ClientShellNotificationEffect>,
    sound_config: &crate::config::SoundConfig,
) {
    for effect in effects {
        match effect {
            shell::ClientShellNotificationEffect::Sound {
                sound,
                agent,
                sound_profile,
            } => {
                if notification_sound_allowed(sound_config, agent.as_deref(), &sound_profile) {
                    crate::sound::play(sound, sound_config);
                }
            }
            shell::ClientShellNotificationEffect::Terminal { title, body } => {
                if let Err(err) = crate::terminal_notify::show_notification(&title, body.as_deref())
                {
                    warn!(err = %err, "failed to emit terminal notification");
                }
            }
            shell::ClientShellNotificationEffect::System { title, body } => {
                if let Err(err) =
                    crate::platform::show_desktop_notification(&title, body.as_deref())
                {
                    warn!(err = %err, "failed to emit system notification");
                }
            }
        }
    }
}

fn notification_sound_allowed(
    config: &crate::config::SoundConfig,
    agent: Option<&str>,
    sound_profile: &shell::ClientNotificationSoundProfile,
) -> bool {
    match sound_profile {
        shell::ClientNotificationSoundProfile::LocalRegistry => {
            config.allows(agent.and_then(|id| crate::detect::Agent::parse(id).ok()))
        }
        shell::ClientNotificationSoundProfile::Resolved(profile) => config.allows_resolved_sound(
            profile.as_ref().map(|profile| profile.config_key.as_str()),
            profile.as_ref().is_some_and(|profile| profile.default_off),
        ),
    }
}

pub(super) fn handle_notify(
    kind: NotifyKind,
    message: &str,
    body: Option<&str>,
    sound_config: &crate::config::SoundConfig,
) {
    handle_notify_with_notifiers(
        kind,
        message,
        body,
        sound_config,
        crate::terminal_notify::show_notification,
        crate::platform::show_desktop_notification,
    );
}

pub(super) fn handle_notify_with_notifiers(
    kind: NotifyKind,
    message: &str,
    body: Option<&str>,
    sound_config: &crate::config::SoundConfig,
    mut show_terminal_notification: impl FnMut(&str, Option<&str>) -> io::Result<bool>,
    mut show_system_notification: impl FnMut(&str, Option<&str>) -> io::Result<bool>,
) {
    match kind {
        NotifyKind::Sound => {
            let Some(sound) = sound_from_notify_message(message) else {
                warn!(
                    message = message,
                    "received unknown sound notification from server"
                );
                return;
            };
            if sound_config.enabled {
                crate::sound::play(sound, sound_config);
            }
        }
        NotifyKind::Toast => {
            debug!(
                message = message,
                "received terminal toast notification from server"
            );
            if let Err(err) = show_terminal_notification(message, body) {
                warn!(err = %err, "failed to emit terminal notification");
            }
        }
        NotifyKind::SystemToast => {
            debug!(
                message = message,
                "received system toast notification from server"
            );
            if let Err(err) = show_system_notification(message, body) {
                warn!(err = %err, "failed to emit system notification");
            }
        }
    }
}

pub(super) fn sound_from_notify_message(message: &str) -> Option<crate::sound::Sound> {
    match message {
        "agent done" => Some(crate::sound::Sound::Done),
        "agent attention" => Some(crate::sound::Sound::Request),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::endpoint::NotificationSoundProfile;
    use shell::ClientNotificationSoundProfile::{LocalRegistry, Resolved};

    #[test]
    fn resolved_sound_overrides_stale_local_registry_without_changing_legacy_fallback() {
        let config = crate::config::SoundConfig::default();
        assert!(!notification_sound_allowed(
            &config,
            Some("droid"),
            &LocalRegistry
        ));
        assert!(notification_sound_allowed(
            &config,
            Some("droid"),
            &Resolved(None)
        ));
        let profile = |off| {
            Resolved(Some(NotificationSoundProfile {
                config_key: "new_remote_key".into(),
                default_off: off,
            }))
        };
        assert!(!notification_sound_allowed(
            &config,
            Some("future-agent"),
            &profile(true)
        ));
        assert!(notification_sound_allowed(
            &config,
            Some("future-agent"),
            &profile(false)
        ));
        let muted = crate::config::SoundConfig {
            enabled: false,
            ..config
        };
        assert!(!notification_sound_allowed(
            &muted,
            Some("future-agent"),
            &profile(false)
        ));
    }

    #[test]
    fn resolved_sound_respects_explicit_local_remote_keys() {
        let config: crate::config::SoundConfig =
            toml::from_str("[agents]\nremote_key = 'on'\nother_key = 'off'\n").unwrap();
        for (key, default_off, expected) in
            [("remote_key", true, true), ("other_key", false, false)]
        {
            let profile = Resolved(Some(NotificationSoundProfile {
                config_key: key.into(),
                default_off,
            }));
            assert_eq!(
                notification_sound_allowed(&config, Some("unknown-remote-agent"), &profile),
                expected
            );
        }
    }
}
