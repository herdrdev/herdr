use std::{collections::BTreeMap, path::Path};

use crate::{detect::Agent, provider_usage::QuotaProvider};

const CLAUDE: &str = "\u{EC82}"; // cod-claude
const OPENAI: &str = "\u{EC81}"; // cod-openai
const KIMI: &str = "\u{F0F65}"; // md-moon_waning_crescent
const PI: &str = "\u{F03FF}"; // md-pi
const GEMINI: &str = "\u{F0AE2}"; // md-star_four_points
const GOOGLE: &str = "\u{F02AD}"; // md-google
const SHELL: &str = "\u{EA85}"; // cod-terminal
const INBOX: &str = "\u{F0687}"; // md-inbox
const IMAGE_MULTIPLE: &str = "\u{F02F9}"; // md-image_multiple
const TMUX: &str = "\u{EBC8}"; // cod-terminal_tmux
const ROBOT: &str = "\u{F06A9}"; // md-robot
const HAMMER_WRENCH: &str = "\u{F1323}"; // md-hammer_wrench
const CONFIG: &str = "\u{E615}"; // seti-config
const OBSIDIAN: &str = "\u{E6BB}"; // custom-obsidian
const REPO: &str = "\u{EA62}"; // cod-repo

pub(crate) fn agent_text_tag(agent: Option<Agent>) -> Option<&'static str> {
    match agent {
        Some(Agent::Codex) => Some("cx"),
        Some(Agent::Claude) => Some("cc"),
        Some(Agent::Pi) => Some("pi"),
        Some(Agent::Kimi) => Some("ki"),
        _ => None,
    }
}

pub(crate) fn agent_icon(agent: Agent) -> Option<&'static str> {
    match agent {
        Agent::Claude => Some(CLAUDE),
        Agent::Codex => Some(OPENAI),
        Agent::Kimi => Some(KIMI),
        Agent::Pi => Some(PI),
        Agent::Gemini => Some(GEMINI),
        Agent::Antigravity => Some(GOOGLE),
        _ => None,
    }
}

pub(crate) fn agent_label(agent: Agent, nerd_font: bool) -> Option<&'static str> {
    if nerd_font {
        agent_icon(agent)
    } else {
        agent_text_tag(Some(agent))
    }
}

pub(crate) fn agent_icon_for_name(name: &str) -> Option<&'static str> {
    let name = name.trim();
    if ["cc", "claude", "claude code"]
        .iter()
        .any(|known| name.eq_ignore_ascii_case(known))
    {
        Some(CLAUDE)
    } else if ["cx", "codex", "openai"]
        .iter()
        .any(|known| name.eq_ignore_ascii_case(known))
    {
        Some(OPENAI)
    } else if ["ki", "kimi"]
        .iter()
        .any(|known| name.eq_ignore_ascii_case(known))
    {
        Some(KIMI)
    } else if name.eq_ignore_ascii_case("pi") {
        Some(PI)
    } else if ["gemini", "gemini cli"]
        .iter()
        .any(|known| name.eq_ignore_ascii_case(known))
    {
        Some(GEMINI)
    } else if ["antigravity", "agy"]
        .iter()
        .any(|known| name.eq_ignore_ascii_case(known))
    {
        Some(GOOGLE)
    } else {
        None
    }
}

pub(crate) fn shell_label(nerd_font: bool) -> &'static str {
    if nerd_font {
        SHELL
    } else {
        ">_"
    }
}

pub(crate) fn usage_label(provider: QuotaProvider, nerd_font: bool) -> &'static str {
    if !nerd_font {
        return match provider {
            QuotaProvider::Claude => "CC",
            QuotaProvider::Codex => "CX",
            QuotaProvider::Kimi => "KI",
            QuotaProvider::Agy => "AG",
        };
    }
    match provider {
        QuotaProvider::Claude => CLAUDE,
        QuotaProvider::Codex => OPENAI,
        QuotaProvider::Kimi => KIMI,
        QuotaProvider::Agy => GOOGLE,
    }
}

pub(crate) fn space_icon<'a>(
    repo_binding: Option<&str>,
    repo_root: Option<&Path>,
    label: &str,
    overrides: &'a BTreeMap<String, String>,
) -> &'a str {
    resolved_space_icon(repo_binding, repo_root, label, overrides).0
}

pub(crate) fn space_badge_icon<'a>(
    repo_binding: Option<&str>,
    repo_root: Option<&Path>,
    label: &str,
    overrides: &'a BTreeMap<String, String>,
) -> Option<&'a str> {
    let (icon, is_specific) = resolved_space_icon(repo_binding, repo_root, label, overrides);
    is_specific.then_some(icon)
}

fn resolved_space_icon<'a>(
    repo_binding: Option<&str>,
    repo_root: Option<&Path>,
    label: &str,
    overrides: &'a BTreeMap<String, String>,
) -> (&'a str, bool) {
    let binding = repo_binding
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let root_name = repo_root
        .and_then(Path::file_name)
        .and_then(|name| name.to_str());
    let binding_name = binding.and_then(|value| value.rsplit(['/', '\\']).next());
    let repo_name = binding_name.or(root_name);

    for candidate in [binding, repo_name, Some(label)].into_iter().flatten() {
        if let Some((_, icon)) = overrides
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(candidate))
        {
            return (icon, true);
        }
    }

    match repo_name {
        Some(name) => builtin_repo_icon(name).map_or((REPO, false), |icon| (icon, true)),
        None => builtin_label_icon(label).map_or((SHELL, false), |icon| (icon, true)),
    }
}

fn builtin_repo_icon(name: &str) -> Option<&'static str> {
    if matches_any(name, &["inbox", "agent-inbox"]) {
        Some(INBOX)
    } else if name.eq_ignore_ascii_case("scalablev2") {
        Some(IMAGE_MULTIPLE)
    } else if name.eq_ignore_ascii_case("herdr") {
        Some(TMUX)
    } else if matches_any(name, &["scalable-agent-fleet", "agent-fleet"]) {
        Some(ROBOT)
    } else if name.eq_ignore_ascii_case("agent-harness") {
        Some(HAMMER_WRENCH)
    } else if name.eq_ignore_ascii_case("dotfiles") {
        Some(CONFIG)
    } else if name.eq_ignore_ascii_case("obsidian-vault") {
        Some(OBSIDIAN)
    } else {
        None
    }
}

fn builtin_label_icon(label: &str) -> Option<&'static str> {
    if matches_any(label, &["inbox", "agent-inbox"]) {
        Some(INBOX)
    } else if label.eq_ignore_ascii_case("scalablev2") {
        Some(IMAGE_MULTIPLE)
    } else if label.eq_ignore_ascii_case("herdr") {
        Some(TMUX)
    } else if matches_any(label, &["scalable-agent-fleet", "agent-fleet"]) {
        Some(ROBOT)
    } else if label.eq_ignore_ascii_case("agent-harness") {
        Some(HAMMER_WRENCH)
    } else if label.eq_ignore_ascii_case("dotfiles") {
        Some(CONFIG)
    } else if label.eq_ignore_ascii_case("obsidian-vault") {
        Some(OBSIDIAN)
    } else {
        None
    }
}

fn matches_any(value: &str, candidates: &[&str]) -> bool {
    candidates
        .iter()
        .any(|candidate| value.eq_ignore_ascii_case(candidate))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_and_provider_icons_match_the_nerd_font_table() {
        for (agent, glyph) in [
            (Agent::Claude, "\u{EC82}"),
            (Agent::Codex, "\u{EC81}"),
            (Agent::Kimi, "\u{F0F65}"),
            (Agent::Pi, "\u{F03FF}"),
            (Agent::Gemini, "\u{F0AE2}"),
            (Agent::Antigravity, "\u{F02AD}"),
        ] {
            assert_eq!(agent_icon(agent), Some(glyph));
        }
        assert_eq!(usage_label(QuotaProvider::Claude, true), "\u{EC82}");
        assert_eq!(usage_label(QuotaProvider::Codex, true), "\u{EC81}");
        assert_eq!(usage_label(QuotaProvider::Kimi, true), "\u{F0F65}");
        assert_eq!(usage_label(QuotaProvider::Agy, true), "\u{F02AD}");
    }

    #[test]
    fn space_icons_cover_builtin_repositories_and_fallbacks() {
        let overrides = BTreeMap::new();
        for (repo, glyph) in [
            ("inbox", "\u{F0687}"),
            ("scalablev2", "\u{F02F9}"),
            ("herdr", "\u{EBC8}"),
            ("scalable-agent-fleet", "\u{F06A9}"),
            ("agent-fleet", "\u{F06A9}"),
            ("agent-harness", "\u{F1323}"),
            ("dotfiles", "\u{E615}"),
            ("obsidian-vault", "\u{E6BB}"),
            ("other-repo", "\u{EA62}"),
        ] {
            assert_eq!(space_icon(Some(repo), None, "ignored", &overrides), glyph);
        }
        assert_eq!(
            space_icon(None, None, "agent-inbox", &overrides),
            "\u{F0687}"
        );
        assert_eq!(
            space_icon(None, None, "plain Space", &overrides),
            "\u{EA85}"
        );
        assert_eq!(
            space_icon(None, Some(Path::new("/work/herdr")), "ignored", &overrides),
            "\u{EBC8}"
        );
    }

    #[test]
    fn custom_space_icon_overrides_repo_and_label_matches_case_insensitively() {
        let overrides = BTreeMap::from([
            ("owner/custom-repo".into(), "◆".into()),
            ("Friendly Space".into(), "◇".into()),
        ]);
        assert_eq!(
            space_icon(
                Some("OWNER/custom-repo"),
                None,
                "Friendly Space",
                &overrides,
            ),
            "◆"
        );
        assert_eq!(space_icon(None, None, "friendly space", &overrides), "◇");

        let built_in_override = BTreeMap::from([("HERDR".into(), "♧".into())]);
        assert_eq!(
            space_icon(Some("owner/herdr"), None, "Herdr", &built_in_override),
            "♧"
        );
        assert_eq!(
            space_badge_icon(Some("owner/herdr"), None, "Herdr", &built_in_override,),
            Some("♧")
        );
        assert_eq!(
            space_badge_icon(Some("owner/unknown"), None, "Unknown", &overrides),
            None
        );
    }

    #[test]
    fn nerd_font_off_keeps_the_text_labels() {
        assert_eq!(agent_label(Agent::Claude, false), Some("cc"));
        assert_eq!(agent_label(Agent::Codex, false), Some("cx"));
        assert_eq!(agent_label(Agent::Pi, false), Some("pi"));
        assert_eq!(agent_label(Agent::Kimi, false), Some("ki"));
        assert_eq!(shell_label(false), ">_");
        assert_eq!(usage_label(QuotaProvider::Claude, false), "CC");
        assert_eq!(usage_label(QuotaProvider::Codex, false), "CX");
        assert_eq!(usage_label(QuotaProvider::Kimi, false), "KI");
        assert_eq!(usage_label(QuotaProvider::Agy, false), "AG");
    }
}
