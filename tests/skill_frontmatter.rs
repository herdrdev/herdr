//! Contract tests for the agent-skill frontmatter in `SKILL.md`.
//!
//! Slash invocation (`/herdr`) resolves by the skill's `name` and always
//! loads the full skill body. Natural-language invocation is different: the
//! only text an agent's router sees before deciding to load the skill is the
//! frontmatter `description`. That single line is the entire trigger surface.
//!
//! These tests pin that surface so it keeps matching how users actually refer
//! to Herdr (including the common "herder" spelling produced by humans and
//! speech-to-text), keeps directive phrasings as exemplars, and keeps a
//! suppression clause for mentions that describe Herdr without directing it.

const SKILL: &str = include_str!("../SKILL.md");

/// Directive phrasings that must be triggerable from the description alone.
/// Each entry pairs a user utterance with the description substrings that
/// give the router a lexical anchor for it.
const DIRECTIVE_CASES: &[(&str, &[&str])] = &[
    ("use herdr to open 3 panes and orchestrate the tasks", &["use herdr", "orchestrate"]),
    ("open this in herdr", &["open this in herdr"]),
    ("have herdr do this", &["have herdr do this"]),
    ("run this with herdr", &["run this with herdr"]),
    ("use the herdr skill", &["use the herdr skill"]),
    ("let herdr manage the panes", &["let herdr manage the panes"]),
    ("have herdr open panes and orchestrate", &["have herdr open panes and orchestrate this"]),
    // Spoken/transcribed alias: users say and STT writes "herder".
    ("have herder split a pane and start codex", &["herder", "split panes"]),
];

/// Descriptive mentions that must not be presented as triggers. The
/// description cannot lexically exclude these, so it must carry an explicit
/// suppression clause covering each verb.
const DESCRIPTIVE_CASES: &[(&str, &str)] = &[
    ("herdr is a terminal multiplexer I heard about", "discussed"),
    ("how does herdr compare to tmux?", "compared"),
    ("I installed herdr yesterday", "installed"),
    ("someone described herdr on the podcast", "described"),
];

fn frontmatter() -> &'static str {
    let rest = SKILL.strip_prefix("---\n").expect("SKILL.md must start with frontmatter");
    let end = rest.find("\n---").expect("frontmatter must be terminated");
    &rest[..end]
}

fn description() -> String {
    let line = frontmatter()
        .lines()
        .find_map(|l| l.strip_prefix("description:"))
        .expect("frontmatter must contain a description");
    line.trim().trim_matches('"').to_lowercase()
}

/// Slash invocation resolves by skill name; it must stay `herdr`.
#[test]
fn skill_name_is_stable_for_slash_invocation() {
    let name = frontmatter()
        .lines()
        .find_map(|l| l.strip_prefix("name:"))
        .expect("frontmatter must contain a name");
    assert_eq!(name.trim(), "herdr");
}

/// Every supported directive phrasing has a lexical anchor in the description.
#[test]
fn description_anchors_directive_phrasings() {
    let desc = description();
    for (utterance, anchors) in DIRECTIVE_CASES {
        for anchor in *anchors {
            assert!(desc.contains(anchor), "no anchor {anchor:?} for {utterance:?}");
        }
    }
}

/// The description names the alias spellings that reach it in practice.
#[test]
fn description_covers_alias_spellings() {
    let desc = description();
    for alias in ["herder", "herdr.dev", "the herdr skill"] {
        assert!(desc.contains(alias), "missing alias {alias:?}");
    }
}

/// The description opens affirmatively and tells the router to act on the
/// mention instead of investigating first.
#[test]
fn description_is_affirmative_and_immediate() {
    let desc = description();
    let leads_with_restriction = desc.starts_with("use only") || desc.starts_with("do not");
    assert!(!leads_with_restriction, "description must not lead with a restriction");
    assert!(desc.contains("invoke on the mention"), "missing immediate-invocation directive");
    let anti_investigation = "do not first ask what herdr is or search for it";
    assert!(desc.contains(anti_investigation), "missing anti-investigation directive");
}

/// Descriptive mentions stay suppressed: the description keeps one negative
/// clause naming each non-directive verb.
#[test]
fn description_suppresses_descriptive_mentions() {
    let desc = description();
    let clause = desc.split("do not use when").nth(1).expect("missing suppression clause");
    for (utterance, verb) in DESCRIPTIVE_CASES {
        assert!(clause.contains(verb), "clause misses {verb:?} for {utterance:?}");
    }
}

/// The environment gate lives in the skill body, where the agent can run it,
/// not in the description, where it reads as a pre-invocation precondition.
#[test]
fn env_gate_is_in_body_not_description() {
    assert!(!description().contains("herdr_env"), "HERDR_ENV must not gate invocation");
    let body = &SKILL[SKILL.find("\n---").expect("frontmatter end") + 4..];
    assert!(body.contains("HERDR_ENV"), "body must keep the HERDR_ENV check");
}
