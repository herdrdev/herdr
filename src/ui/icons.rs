use std::{collections::BTreeMap, path::Path};

use ratatui::style::Color;

use crate::{app::state::Palette, detect::Agent, provider_usage::QuotaProvider};

/// Claude's brand orange, one step brighter on dark themes.
pub(crate) fn claude_color(p: &Palette) -> Color {
    if is_dark(p) {
        Color::Rgb(240, 122, 53)
    } else {
        Color::Rgb(217, 97, 31)
    }
}

/// Codex follows OpenAI's monochrome mark: the theme's main text colour, so it
/// is black on light themes and stays legible on dark ones.
pub(crate) fn codex_color(p: &Palette) -> Color {
    p.text
}

fn is_dark(p: &Palette) -> bool {
    p.appearance() == Some(crate::terminal_theme::HostAppearance::Dark)
}

/// Swaps an icon for its dark-theme variant where the brand mark has one.
pub(crate) fn themed<'a>(icon: &'a str, p: &Palette) -> &'a str {
    if icon == SCALABLE && is_dark(p) {
        SCALABLE_DARK
    } else {
        icon
    }
}

const CLAUDE: &str = "\u{EC82}"; // cod-claude
const OPENAI: &str = "\u{EC81}"; // cod-openai
const PI: &str = "\u{F03FF}"; // md-pi
const GEMINI: &str = "\u{F0AE2}"; // md-star_four_points
                                  // Brand marks from the Herdr Icons font (dotfiles wezterm/fonts), traced from
                                  // each site's favicon.
const ANTIGRAVITY: &str = "\u{F6000}";
const KIMI: &str = "\u{F6001}";
const SCALABLE: &str = "\u{F6002}";
const SCALABLE_DARK: &str = "\u{F6007}";
const HERDR: &str = "\u{F6003}";
const INBOX: &str = "\u{F6004}";
const OPENCODE: &str = "\u{F6005}";
pub(crate) const GITHUB: &str = "\u{F6006}";
pub(crate) const SENTRY: &str = "\u{F6008}";
pub(crate) const LINEAR: &str = "\u{F6009}";
const SHELL: &str = "\u{EA85}"; // cod-terminal
const ROBOT: &str = "\u{F06A9}"; // md-robot
const HAMMER_WRENCH: &str = "\u{F1323}"; // md-hammer_wrench
const CONFIG: &str = "\u{E615}"; // seti-config
const OBSIDIAN: &str = "\u{E6BB}"; // custom-obsidian
const REPO: &str = "\u{EA62}"; // cod-repo
const CYCLE: &str = "\u{F021}"; // fa-refresh

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
        Agent::Antigravity => Some(ANTIGRAVITY),
        Agent::OpenCode => Some(OPENCODE),
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
        Some(ANTIGRAVITY)
    } else if name.eq_ignore_ascii_case("opencode") {
        Some(OPENCODE)
    } else {
        None
    }
}

pub(crate) fn cycle_label(nerd_font: bool) -> &'static str {
    if nerd_font {
        CYCLE
    } else {
        "cy"
    }
}

const CHIP: &str = "\u{F061A}"; // md-chip
const MEMORY: &str = "\u{F035B}"; // md-memory
const HARDDISK: &str = "\u{F02CA}"; // md-harddisk
const FOLDER_OPEN: &str = "\u{F07C}"; // fa-folder_open
const LIGHTBULB_ON_OUTLINE: &str = "\u{F0A00}"; // md-lightbulb_on_outline

/// Host metrics shown in the status row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Metric {
    Cpu,
    Mem,
    Dsk,
}

pub(crate) fn metric_label(metric: Metric, nerd_font: bool) -> &'static str {
    match (metric, nerd_font) {
        (Metric::Cpu, true) => CHIP,
        (Metric::Mem, true) => MEMORY,
        (Metric::Dsk, true) => HARDDISK,
        (Metric::Cpu, false) => "CPU",
        (Metric::Mem, false) => "MEM",
        (Metric::Dsk, false) => "DSK",
    }
}

/// Tab-row "open repo in editor" button label, padded one cell each side.
pub(crate) fn repo_editor_button_label(nerd_font: bool) -> String {
    if nerd_font {
        format!(" {FOLDER_OPEN} ")
    } else {
        " nvim ".to_string()
    }
}

/// Tab-row "add action" button label, padded one cell each side.
pub(crate) fn add_action_button_label(nerd_font: bool) -> String {
    if nerd_font {
        format!(" {LIGHTBULB_ON_OUTLINE} ")
    } else {
        " + Action ".to_string()
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
        QuotaProvider::Agy => ANTIGRAVITY,
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

    let exact = match repo_name {
        Some(name) => builtin_repo_icon(name),
        None => builtin_label_icon(label),
    };
    if let Some(icon) = exact {
        return (icon, true);
    }
    // The visible title wins over the repo name; both are checked.
    if let Some(icon) = keyword_icon(label).or_else(|| repo_name.and_then(keyword_icon)) {
        return (icon, true);
    }
    (if repo_name.is_some() { REPO } else { SHELL }, false)
}

const KEYWORD_ICONS: &[(&str, &str)] = &[
    ("herdr", HERDR),
    ("scalable", SCALABLE),
    ("scalablev2", SCALABLE),
    ("github", GITHUB),
    ("gh", GITHUB),
    ("pr", GITHUB),
    ("inbox", INBOX),
    ("obsidian", OBSIDIAN),
    ("obs", OBSIDIAN),
    ("fleet", ROBOT),
    ("harness", HAMMER_WRENCH),
    ("dotfiles", CONFIG),
    ("config", CONFIG),
    ("sentry", SENTRY),
    ("linear", LINEAR),
];

/// Matches the earliest whole keyword without allocating, for render-path use.
pub(crate) fn keyword_icon(title: &str) -> Option<&'static str> {
    title
        .split(|character: char| !character.is_alphanumeric())
        .find_map(|word| {
            KEYWORD_ICONS
                .iter()
                .find_map(|(keyword, icon)| word.eq_ignore_ascii_case(keyword).then_some(*icon))
        })
}

pub(crate) fn keyword_icon_matches(title: &str, icon: &str) -> bool {
    title
        .split(|character: char| !character.is_alphanumeric())
        .any(|word| {
            KEYWORD_ICONS.iter().any(|(keyword, matched)| {
                word.eq_ignore_ascii_case(keyword)
                    && (*matched == icon || (*matched == SCALABLE && icon == SCALABLE_DARK))
            })
        })
}

/// Returns the leading private-use glyph, allowing a tab pin before the label.
/// Fleet brand prefixes and Nerd Font labels both use private-use codepoints.
pub(crate) fn leading_label_icon(label: &str) -> Option<char> {
    let mut chars = label.chars().peekable();
    while chars
        .peek()
        .is_some_and(|character| character.is_whitespace())
    {
        chars.next();
    }
    if chars.peek() == Some(&'*') {
        chars.next();
        while chars
            .peek()
            .is_some_and(|character| character.is_whitespace())
        {
            chars.next();
        }
    }
    chars
        .find(|character| !character.is_whitespace())
        .filter(|character| matches!(*character as u32, 0xE000..=0xF8FF | 0xF0000..=0x10FFFF))
}

pub(crate) fn badge_icon_for_label<'a>(label: &str, icon: &'a str) -> Option<&'a str> {
    leading_label_icon(label).is_none().then_some(icon)
}

pub(crate) fn child_badge_icon<'a>(
    parent_label: &str,
    parent_icon: &str,
    child_icon: &'a str,
) -> Option<&'a str> {
    let parent_glyph = leading_label_icon(parent_label);
    let child_glyph = child_icon.chars().next();
    (parent_glyph != child_glyph
        && !keyword_icon_matches(parent_label, child_icon)
        && (parent_glyph.is_some() || parent_icon != child_icon))
        .then_some(child_icon)
}

fn builtin_repo_icon(name: &str) -> Option<&'static str> {
    if matches_any(name, &["inbox", "agent-inbox"]) {
        Some(INBOX)
    } else if name.eq_ignore_ascii_case("scalablev2") {
        Some(SCALABLE)
    } else if name.eq_ignore_ascii_case("herdr") {
        Some(HERDR)
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
        Some(SCALABLE)
    } else if label.eq_ignore_ascii_case("herdr") {
        Some(HERDR)
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
            (Agent::Kimi, "\u{F6001}"),
            (Agent::Pi, "\u{F03FF}"),
            (Agent::Gemini, "\u{F0AE2}"),
            (Agent::Antigravity, "\u{F6000}"),
            (Agent::OpenCode, "\u{F6005}"),
        ] {
            assert_eq!(agent_icon(agent), Some(glyph));
        }
        assert_eq!(usage_label(QuotaProvider::Claude, true), "\u{EC82}");
        assert_eq!(usage_label(QuotaProvider::Codex, true), "\u{EC81}");
        assert_eq!(usage_label(QuotaProvider::Kimi, true), "\u{F6001}");
        assert_eq!(usage_label(QuotaProvider::Agy, true), "\u{F6000}");
    }

    #[test]
    fn space_icons_cover_builtin_repositories_and_fallbacks() {
        let overrides = BTreeMap::new();
        for (repo, glyph) in [
            ("inbox", "\u{F6004}"),
            ("scalablev2", "\u{F6002}"),
            ("herdr", "\u{F6003}"),
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
            "\u{F6004}"
        );
        assert_eq!(
            space_icon(None, None, "plain Space", &overrides),
            "\u{EA85}"
        );
        assert_eq!(
            space_icon(None, Some(Path::new("/work/herdr")), "ignored", &overrides),
            "\u{F6003}"
        );
    }

    #[test]
    fn keyword_icons_match_whole_words_case_insensitively_in_title_order() {
        assert_eq!(keyword_icon("a NONLINEAR repo"), None);
        assert_eq!(keyword_icon("linearized"), None);
        assert_eq!(keyword_icon("Linear issue for Sentry"), Some(LINEAR));
        assert_eq!(keyword_icon("prefix/GH-42"), Some(GITHUB));
        assert_eq!(keyword_icon("Scalable V2"), Some(SCALABLE));
        assert_eq!(keyword_icon("SENTRY"), Some(SENTRY));
    }

    #[test]
    fn label_glyphs_replace_badges_and_only_matching_child_icons_are_suppressed() {
        let brand = '\u{F6002}';
        let brand_label = format!("  * {brand} Scalable");
        assert_eq!(leading_label_icon(&brand_label), Some(brand));
        assert_eq!(badge_icon_for_label(&brand_label, SCALABLE), None);
        assert_eq!(
            badge_icon_for_label("plain workspace", SCALABLE),
            Some(SCALABLE)
        );
        assert_eq!(leading_label_icon("* \u{E6BB} inbox"), Some('\u{E6BB}'));
        assert_eq!(leading_label_icon("plain"), None);

        assert_eq!(child_badge_icon(&brand_label, SCALABLE, SCALABLE), None);
        assert_eq!(child_badge_icon(&brand_label, SCALABLE, INBOX), Some(INBOX));
        assert_eq!(
            child_badge_icon("Scalable workspace", SCALABLE, SCALABLE),
            None
        );
        assert_eq!(
            child_badge_icon("Other workspace", SCALABLE, INBOX),
            Some(INBOX)
        );
    }

    #[test]
    fn space_keyword_icons_follow_exact_matches_and_user_overrides() {
        let no_overrides = BTreeMap::new();
        assert_eq!(
            space_icon(Some("owner/linear-work"), None, "Sentry", &no_overrides),
            SENTRY,
            "visible title keyword precedes repo keyword"
        );
        assert_eq!(
            space_icon(Some("owner/linear-work"), None, "scratch", &no_overrides),
            LINEAR,
            "repo keyword applies when the title has none"
        );
        assert_eq!(
            space_icon(None, None, "Sentry Linear", &no_overrides),
            SENTRY,
            "first keyword by position wins"
        );
        assert_eq!(
            space_icon(Some("owner/agent-fleet"), None, "other", &no_overrides),
            ROBOT,
            "exact built-in result is preserved"
        );
        let overrides = BTreeMap::from([("owner/linear-work".into(), "◆".into())]);
        assert_eq!(
            space_icon(Some("owner/linear-work"), None, "other", &overrides),
            "◆"
        );
    }

    #[test]
    fn workspace_keyword_icons_retain_dark_scalable_theming() {
        let overrides = BTreeMap::new();
        assert_eq!(
            space_icon(Some("owner/scalable-platform"), None, "other", &overrides),
            SCALABLE
        );
        let mut palette = crate::app::state::AppState::test_new().palette;
        palette.panel_bg = Color::Rgb(30, 30, 46);
        assert_eq!(themed(SCALABLE, &palette), SCALABLE_DARK);
    }

    #[test]
    fn scalable_icon_swaps_to_its_bordered_variant_on_dark_themes() {
        let mut palette = crate::app::state::AppState::test_new().palette;
        palette.panel_bg = Color::Rgb(250, 250, 250);
        assert_eq!(themed(SCALABLE, &palette), SCALABLE);
        assert_eq!(claude_color(&palette), Color::Rgb(217, 97, 31));
        palette.panel_bg = Color::Rgb(30, 30, 46);
        assert_eq!(themed(SCALABLE, &palette), SCALABLE_DARK);
        assert_eq!(themed(HERDR, &palette), HERDR);
        assert_eq!(claude_color(&palette), Color::Rgb(240, 122, 53));
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
    fn fleet_workspace_ac6_cycle_icon_uses_nerd_font_and_ascii_fallback() {
        assert_eq!(agent_label(Agent::Claude, false), Some("cc"));
        assert_eq!(agent_label(Agent::Codex, false), Some("cx"));
        assert_eq!(agent_label(Agent::Pi, false), Some("pi"));
        assert_eq!(agent_label(Agent::Kimi, false), Some("ki"));
        assert_eq!(usage_label(QuotaProvider::Claude, false), "CC");
        assert_eq!(usage_label(QuotaProvider::Codex, false), "CX");
        assert_eq!(usage_label(QuotaProvider::Kimi, false), "KI");
        assert_eq!(usage_label(QuotaProvider::Agy, false), "AG");
        assert_eq!(cycle_label(false), "cy");
        assert_eq!(cycle_label(true), "\u{F021}");
    }
}
