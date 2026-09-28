use super::*;

// Codex is only a registry key here; behavior tests supply synthetic rules.
fn remote_manifest(version: &str, state: &str, contains: &str) -> String {
    format!(
        r#"
id = "codex"
version = "{version}"
min_engine_version = 1
updated_at = "2026-06-10T12:00:00Z"

[[rules]]
id = "test"
state = "{state}"
contains = ["{contains}"]
"#
    )
}

fn local_manifest(state: &str, contains: &str) -> String {
    format!(
        r#"
id = "codex"

[[rules]]
id = "test"
state = "{state}"
contains = ["{contains}"]
"#
    )
}

fn rules_manifest(rules: &str) -> String {
    format!(
        r#"
id = "codex"

{rules}
"#
    )
}

fn with_manifest_dirs<T>(name: &str, f: impl FnOnce() -> T) -> T {
    with_bundled_manifests(name, f)
}

/// `explain` against the bundled manifests only. See `with_bundled_manifests`:
/// without it these assertions read whatever manifest the developer's machine
/// last downloaded.
fn bundled_explain(agent: Agent, screen: &str) -> DetectionExplain {
    with_bundled_manifests("bundled-explain", || explain(agent, screen))
}

fn fixture_with_banner(fixture: &str, banner: &str) -> String {
    let (_, tail) = fixture.split_once('\n').unwrap();
    format!("{banner}\n{tail}")
}

fn write_remote_codex(content: &str) {
    let path = crate::detect::manifest_update::remote_manifest_path(Agent::Codex);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
    reload_manifests();
}

fn write_remote_codex_without_reload(content: &str) {
    let path = crate::detect::manifest_update::remote_manifest_path(Agent::Codex);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn write_local_codex(content: &str) {
    let path = override_path(Agent::Codex).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
    reload_manifests();
}

#[test]
fn known_agent_no_match_defaults_to_idle_fallback() {
    with_manifest_dirs("no-match", || {
        write_local_codex(&local_manifest("working", "active-marker"));
        let explain = explain(Agent::Codex, "unmatched-marker");

        assert_eq!(explain.state, AgentState::Idle);
        assert!(!explain.visible_idle);
        assert_eq!(
            explain.fallback_reason.as_deref(),
            Some(DEFAULT_KNOWN_AGENT_IDLE_FALLBACK)
        );
    });
}

#[test]
fn rule_semantics_apply_gates_priority_and_line_regex() {
    with_manifest_dirs("rule-semantics", || {
        write_local_codex(&rules_manifest(
            r#"
[[rules]]
id = "low_contains"
state = "idle"
priority = 1
contains = ["match"]

[[rules]]
id = "high_nested_gates"
state = "working"
priority = 10
contains = ["match"]
all = [
  { any = [{ regex = ["w[io]n"] }, { contains = ["fallback"] }] },
]
not = [
  { contains = ["blocked"] },
]

[[rules]]
id = "line_regex"
state = "blocked"
priority = 20
line_regex = ["^exact line$"]
"#,
        ));

        let high = explain(Agent::Codex, "match win");
        assert_eq!(high.state, AgentState::Working);
        assert_eq!(
            high.matched_rule.as_ref().map(|rule| rule.id.as_str()),
            Some("high_nested_gates")
        );

        let not_gate = explain(Agent::Codex, "match win blocked");
        assert_eq!(not_gate.state, AgentState::Idle);
        assert_eq!(
            not_gate.matched_rule.as_ref().map(|rule| rule.id.as_str()),
            Some("low_contains")
        );

        let line = explain(Agent::Codex, "before\nexact line\nafter");
        assert_eq!(line.state, AgentState::Blocked);
        assert_eq!(
            line.matched_rule.as_ref().map(|rule| rule.id.as_str()),
            Some("line_regex")
        );
    });
}

#[test]
fn remote_manifest_loads_between_local_override_and_bundled() {
    with_manifest_dirs("remote-source", || {
        write_remote_codex(&remote_manifest("9999.01.01.1", "blocked", "remote-ready"));

        let explain = explain(Agent::Codex, "remote-ready");

        assert_eq!(explain.state, AgentState::Blocked);
        assert!(matches!(
            explain.source,
            Some(ManifestSource::Remote { .. })
        ));
        assert_eq!(explain.manifest_version.as_deref(), Some("9999.01.01.1"));
        assert_eq!(
            explain.cached_remote_version.as_deref(),
            Some("9999.01.01.1")
        );
    });
}

#[test]
fn fallback_explain_preserves_active_manifest_version() {
    with_manifest_dirs("fallback-version", || {
        write_remote_codex(&remote_manifest("9999.01.01.1", "blocked", "remote-ready"));

        let explain = explain(Agent::Codex, "ordinary prompt text");

        assert_eq!(explain.state, AgentState::Idle);
        assert_eq!(
            explain.fallback_reason.as_deref(),
            Some(DEFAULT_KNOWN_AGENT_IDLE_FALLBACK)
        );
        assert_eq!(explain.manifest_version.as_deref(), Some("9999.01.01.1"));
        assert!(matches!(
            explain.source,
            Some(ManifestSource::Remote { .. })
        ));
    });
}

#[test]
fn older_cached_remote_manifest_does_not_shadow_newer_bundled_manifest() {
    with_manifest_dirs("older-remote-bundled-fallback", || {
        write_remote_codex(&remote_manifest("2026.06.10.0", "blocked", "remote-ready"));

        let explain = explain(Agent::Codex, "remote-ready");

        assert!(matches!(explain.source, Some(ManifestSource::Bundled)));
        assert_eq!(
            explain.cached_remote_version.as_deref(),
            Some("2026.06.10.0")
        );
        assert!(explain
            .warning
            .as_deref()
            .is_some_and(|warning| warning.contains("older than bundled")));
    });
}

// A synthetic bundled manifest passed straight to `read_remote_manifest`, so
// the overlay tests do not depend on any real agent's rule ids.
fn overlay_bundled_manifest(version: &str) -> AgentManifest {
    parse_manifest(&format!(
        r#"
id = "codex"
version = "{version}"
min_engine_version = 1

[[rules]]
id = "shared"
state = "working"
contains = ["bundled-shared"]

[[rules]]
id = "fork_only_blocked"
state = "blocked"
priority = 50
visible_blocker = true
contains = ["fork-only-blocker"]
"#
    ))
    .unwrap()
}

fn overlay_remote_manifest(version: &str, extra_rules: &str) -> String {
    format!(
        r#"
id = "codex"
version = "{version}"
min_engine_version = 1
updated_at = "2026-06-10T12:00:00Z"

[[rules]]
id = "shared"
state = "idle"
contains = ["remote-shared"]
{extra_rules}
"#
    )
}

fn overlay_explain(loaded: LoadedManifest, screen: &str) -> DetectionExplain {
    evaluate_loaded_manifest(
        Agent::Codex,
        DetectionInput {
            screen,
            osc_title: "",
            osc_progress: "",
        },
        loaded,
        false,
    )
}

#[test]
fn newer_remote_manifest_keeps_bundled_rules_it_does_not_define() {
    with_manifest_dirs("overlay-fork-only", || {
        write_remote_codex(&overlay_remote_manifest("9999.01.01.1", ""));
        let bundled = overlay_bundled_manifest("2026.01.01.1");

        let loaded = read_remote_manifest(Agent::Codex, &bundled).unwrap();
        assert!(matches!(loaded.source, ManifestSource::Remote { .. }));
        assert_eq!(loaded.overlaid_rule_ids, vec!["fork_only_blocked"]);

        let blocked = overlay_explain(loaded.clone(), "fork-only-blocker");
        assert_eq!(blocked.state, AgentState::Blocked);
        assert!(blocked.visible_blocker);
        assert_eq!(
            blocked.matched_rule.as_ref().map(|rule| rule.id.as_str()),
            Some("fork_only_blocked")
        );
        assert_eq!(blocked.overlaid_rule_ids, vec!["fork_only_blocked"]);

        // The shared id comes from the remote, not the bundled copy.
        let shared = overlay_explain(loaded.clone(), "remote-shared");
        assert_eq!(shared.state, AgentState::Idle);
        let bundled_shared = overlay_explain(loaded, "bundled-shared");
        assert!(bundled_shared.matched_rule.is_none());
    });
}

#[test]
fn newer_remote_manifest_rule_with_same_id_replaces_bundled_rule() {
    with_manifest_dirs("overlay-same-id", || {
        write_remote_codex(&overlay_remote_manifest(
            "9999.01.01.1",
            r#"
[[rules]]
id = "fork_only_blocked"
state = "blocked"
regex = ['^remote-[a-z]+-blocker$']
"#,
        ));
        let bundled = overlay_bundled_manifest("2026.01.01.1");

        let loaded = read_remote_manifest(Agent::Codex, &bundled).unwrap();
        assert!(loaded.overlaid_rule_ids.is_empty());

        let bundled_screen = overlay_explain(loaded.clone(), "fork-only-blocker");
        assert!(bundled_screen.matched_rule.is_none());
        let remote_screen = overlay_explain(loaded, "remote-only-blocker");
        assert_eq!(remote_screen.state, AgentState::Blocked);
        assert_eq!(
            remote_screen
                .matched_rule
                .as_ref()
                .map(|rule| rule.id.as_str()),
            Some("fork_only_blocked")
        );
        assert!(remote_screen.overlaid_rule_ids.is_empty());
    });
}

#[test]
fn older_remote_manifest_uses_bundled_without_overlay() {
    with_manifest_dirs("overlay-older-remote", || {
        write_remote_codex(&overlay_remote_manifest("2026.01.01.1", ""));
        let bundled = overlay_bundled_manifest("9999.01.01.1");

        let loaded = read_remote_manifest(Agent::Codex, &bundled).unwrap();
        assert!(matches!(loaded.source, ManifestSource::Bundled));
        assert!(loaded.overlaid_rule_ids.is_empty());
        assert!(loaded
            .warning
            .as_deref()
            .is_some_and(|warning| warning.contains("older than bundled")));

        let shared = overlay_explain(loaded, "bundled-shared");
        assert_eq!(shared.state, AgentState::Working);
    });
}

#[test]
fn explain_json_lists_overlaid_rule_ids_only_when_present() {
    with_manifest_dirs("overlay-explain-json", || {
        write_remote_codex(&overlay_remote_manifest("9999.01.01.1", ""));
        let overlaid =
            read_remote_manifest(Agent::Codex, &overlay_bundled_manifest("2026.01.01.1")).unwrap();
        let json = explain_to_json_value(&overlay_explain(overlaid, "fork-only-blocker"));
        assert_eq!(
            json["overlaid_rule_ids"],
            serde_json::json!(["fork_only_blocked"])
        );

        let bundled =
            read_remote_manifest(Agent::Codex, &overlay_bundled_manifest("9999.01.01.2")).unwrap();
        let json = explain_to_json_value(&overlay_explain(bundled, "bundled-shared"));
        assert!(json.get("overlaid_rule_ids").is_none());
    });
}

#[test]
fn cache_reload_applies_overlay_to_downloaded_remote_manifest() {
    // Goes through the real reload path and bundled codex manifest, but only
    // checks that the overlay happened, not which bundled rule ids exist.
    with_manifest_dirs("overlay-cache-reload", || {
        write_remote_codex(&remote_manifest("9999.01.01.1", "blocked", "remote-ready"));

        let explain = explain(Agent::Codex, "remote-ready");
        assert!(matches!(
            explain.source,
            Some(ManifestSource::Remote { .. })
        ));
        assert!(!explain.overlaid_rule_ids.is_empty());
        assert!(!explain.overlaid_rule_ids.iter().any(|id| id == "test"));

        let summary = manifest_summaries()
            .into_iter()
            .find(|summary| summary.agent == Agent::Codex)
            .unwrap();
        assert_eq!(summary.overlaid_rule_ids, explain.overlaid_rule_ids);
    });
}

#[test]
fn local_override_shadows_cached_remote_manifest() {
    with_manifest_dirs("local-shadows-remote", || {
        write_remote_codex(&remote_manifest("9999.01.01.1", "blocked", "remote-ready"));
        write_local_codex(&local_manifest("idle", "local-ready"));

        let explain = explain(Agent::Codex, "local-ready");

        assert_eq!(explain.state, AgentState::Idle);
        assert!(matches!(explain.source, Some(ManifestSource::Override(_))));
        assert!(explain.local_override_shadowing_remote);
        assert_eq!(
            explain.cached_remote_version.as_deref(),
            Some("9999.01.01.1")
        );
    });
}

#[test]
fn invalid_local_override_falls_back_to_cached_remote_manifest() {
    with_manifest_dirs("invalid-local-remote-fallback", || {
        write_remote_codex(&remote_manifest("9999.01.01.1", "blocked", "remote-ready"));
        write_local_codex("id = ");

        let explain = explain(Agent::Codex, "remote-ready");

        assert_eq!(explain.state, AgentState::Blocked);
        assert!(matches!(
            explain.source,
            Some(ManifestSource::Remote { .. })
        ));
        assert!(explain.warning.is_some());
    });
}

#[test]
fn detection_uses_cached_manifest_until_explicit_reload() {
    with_manifest_dirs("cache-boundary", || {
        write_remote_codex(&remote_manifest("9999.01.01.1", "blocked", "cached-ready"));

        let cached = explain(Agent::Codex, "cached-ready");
        assert_eq!(cached.state, AgentState::Blocked);
        assert!(matches!(cached.source, Some(ManifestSource::Remote { .. })));
        assert_eq!(
            cached.matched_rule.as_ref().map(|rule| rule.id.as_str()),
            Some("test")
        );

        write_remote_codex_without_reload(&remote_manifest("9999.01.01.2", "working", "new-ready"));

        let unchanged = explain(Agent::Codex, "new-ready");
        assert_eq!(unchanged.state, AgentState::Idle);
        assert_eq!(
            unchanged.fallback_reason.as_deref(),
            Some(DEFAULT_KNOWN_AGENT_IDLE_FALLBACK)
        );
        assert_eq!(
            unchanged.cached_remote_version.as_deref(),
            Some("9999.01.01.1")
        );

        reload_manifests();

        let reloaded = explain(Agent::Codex, "new-ready");
        assert_eq!(reloaded.state, AgentState::Working);
        assert_eq!(
            reloaded.cached_remote_version.as_deref(),
            Some("9999.01.01.2")
        );
        assert_eq!(
            reloaded.matched_rule.as_ref().map(|rule| rule.id.as_str()),
            Some("test")
        );
    });
}

#[test]
fn compiled_rules_are_shared_until_manifest_reload() {
    with_manifest_dirs("shared-compiled-rules", || {
        write_remote_codex(&format!(
            "{}\nregex = ['^cached-[a-z]+$']\n",
            remote_manifest("9999.01.01.1", "blocked", "cached-ready")
        ));
        let first = load_manifest(Agent::Codex).unwrap();
        let second = load_manifest(Agent::Codex).unwrap();
        assert!(!first.compiled_rules.is_empty());
        assert_eq!(
            first.compiled_rules.as_ptr(),
            second.compiled_rules.as_ptr(),
            "cached loads must retain the same compiled rules and regex search caches"
        );

        write_remote_codex_without_reload(&format!(
            "{}\nregex = ['^new-[a-z]+$']\n",
            remote_manifest("9999.01.01.2", "working", "new-ready")
        ));
        let unchanged = load_manifest(Agent::Codex).unwrap();
        assert_eq!(
            first.compiled_rules.as_ptr(),
            unchanged.compiled_rules.as_ptr()
        );

        reload_manifests_for_agents(&[Agent::Codex]);
        let reloaded = load_manifest(Agent::Codex).unwrap();
        let shared_reload = load_manifest(Agent::Codex).unwrap();
        assert_ne!(
            first.compiled_rules.as_ptr(),
            reloaded.compiled_rules.as_ptr()
        );
        assert_eq!(
            reloaded.compiled_rules.as_ptr(),
            shared_reload.compiled_rules.as_ptr()
        );
        assert!(compiled_rule_matches(
            &first.compiled_rules[0],
            "cached-ready"
        ));
        assert!(!compiled_rule_matches(
            &first.compiled_rules[0],
            "new-ready"
        ));
        assert_eq!(
            explain(Agent::Codex, "new-ready").state,
            AgentState::Working
        );

        std::thread::scope(|scope| {
            for _ in 0..4 {
                let reloaded = &reloaded;
                scope.spawn(move || {
                    let loaded = load_manifest(Agent::Codex).unwrap();
                    assert_eq!(
                        loaded.compiled_rules.as_ptr(),
                        reloaded.compiled_rules.as_ptr()
                    );
                    for _ in 0..8 {
                        assert_eq!(detect(Agent::Codex, "new-ready").state, AgentState::Working);
                    }
                });
            }
        });
    });
}

#[test]
fn osc_regions_use_separate_inputs_and_share_rule_priority() {
    with_manifest_dirs("osc-regions", || {
        write_local_codex(&rules_manifest(
            r#"
[[rules]]
id = "screen"
state = "idle"
priority = 10
region = "whole_recent"
visible_idle = true
contains = ["screen-marker"]

[[rules]]
id = "title"
state = "working"
priority = 20
region = "osc_title"
visible_working = true
regex = ['^title-marker$']

[[rules]]
id = "progress"
state = "blocked"
priority = 30
region = "osc_progress"
visible_blocker = true
regex = ['^progress-marker$']
"#,
        ));
        for (screen, title, progress, state, rule) in [
            ("screen-marker", "", "", AgentState::Idle, "screen"),
            (
                "screen-marker",
                "title-marker",
                "",
                AgentState::Working,
                "title",
            ),
            (
                "screen-marker",
                "title-marker",
                "progress-marker",
                AgentState::Blocked,
                "progress",
            ),
            (
                "screen-marker title-marker progress-marker",
                "",
                "",
                AgentState::Idle,
                "screen",
            ),
        ] {
            let input = DetectionInput {
                screen,
                osc_title: title,
                osc_progress: progress,
            };
            let result = explain_with_input(Agent::Codex, input);
            assert_eq!(result.state, state);
            assert_eq!(
                result
                    .matched_rule
                    .as_ref()
                    .map(|matched| matched.id.as_str()),
                Some(rule)
            );
            let detection =
                crate::detect::detect_agent_with_osc(Some(Agent::Codex), screen, title, progress);
            assert_eq!(detection.state, state);
            assert_eq!(detection.visible_idle, state == AgentState::Idle);
            assert_eq!(detection.visible_working, state == AgentState::Working);
            assert_eq!(detection.visible_blocker, state == AgentState::Blocked);
        }
        let swapped = explain_with_input(
            Agent::Codex,
            DetectionInput {
                screen: "",
                osc_title: "progress-marker",
                osc_progress: "title-marker",
            },
        );
        assert!(swapped.matched_rule.is_none());
    });
}

#[test]
fn skip_rule_suppresses_state_update_without_visible_state_evidence() {
    with_manifest_dirs("skip-rule", || {
        write_local_codex(&rules_manifest(
            r#"
[[rules]]
id = "activity"
state = "working"
priority = 10
visible_working = true
contains = ["activity-marker"]

[[rules]]
id = "overlay"
state = "unknown"
priority = 20
skip_state_update = true
contains = ["overlay-marker"]
"#,
        ));
        let screen = "activity-marker overlay-marker";
        let result = explain(Agent::Codex, screen);
        assert_eq!(result.state, AgentState::Unknown);
        assert!(result.skip_state_update);
        assert_eq!(
            result.skipped_update_reason.as_deref(),
            Some("matched_rule:overlay")
        );
        assert!(!result.visible_idle);
        assert!(!result.visible_working);
        assert!(!result.visible_blocker);
        assert!(detect(Agent::Codex, screen).skip_state_update);
        assert!(should_skip_state_update(Agent::Codex, screen));
        assert!(!should_skip_state_update(Agent::Codex, "activity-marker"));
    });
}

#[test]
fn screen_regions_extract_structure_without_classifying_agent_state() {
    for (screen, spec, expected) in [
        ("old\n\nnew\n", "bottom_lines(2)", "\nnew\n"),
        (
            "before\n› input\nafter\n",
            "after_last_prompt_marker",
            "after\n",
        ),
        (
            "before\n› input\nafter\n",
            "before_current_prompt_marker",
            "before\n",
        ),
        (
            "before\n› input\nafter\n",
            "whole_recent_without_current_prompt_marker",
            "",
        ),
        (
            "no marker\n",
            "whole_recent_without_current_prompt_marker",
            "no marker\n",
        ),
        (
            "• old\n■ latest\n› input\n",
            "current_prompt_block_marker",
            "■ latest",
        ),
        (
            "• old\n■ latest\n› input\n",
            "after_current_prompt_block_marker",
            "■ latest\n› input\n",
        ),
        ("› old\n• new\n", "current_prompt_block_marker", ""),
        (
            "above\n\n───\nbody\n───\nfooter\n",
            "above_prompt_box",
            "above\n\n",
        ),
        (
            "above\n\n───\nbody\n───\nfooter\n",
            "last_non_empty_above_prompt_box",
            "above",
        ),
        (
            "above\n───\nbody\n───\nfooter\n",
            "prompt_box_body",
            "body\n",
        ),
        (
            "above\n───\nbody\n───\nfooter\n",
            "after_last_horizontal_rule",
            "footer\n",
        ),
    ] {
        assert_eq!(
            region(
                DetectionInput {
                    screen,
                    osc_title: "",
                    osc_progress: ""
                },
                spec
            ),
            expected,
            "region={spec}"
        );
    }
}

#[test]
fn all_bundled_manifests_parse_and_validate() {
    for agent in Agent::SCREEN_MANIFEST_AGENTS {
        assert!(
            bundled_manifest(agent).is_some(),
            "missing bundled manifest for {}",
            agent_label(agent)
        );
    }
}

#[test]
fn devin_manifest_detects_idle_working_and_blocked_states() {
    let idle = bundled_explain(
        Agent::Devin,
        "─────────────────────────────────────────────────────\n❭ Ask Devin to build features, fix bugs, or work on\n  your code\n─────────────────────────────────────────────────────\nSWE-1.6               Context: 16k / 200k tokens (7%)",
    );
    assert_eq!(idle.state, AgentState::Idle);
    assert!(idle.visible_idle);

    let live_footer_idle = bundled_explain(
        Agent::Devin,
        "Done.\n\n────────────────────────────────────────────────── (bypass permissions on) ─\n❭\n────────────────────────────────────────────────────────────────────────────\nClaude Opus 4.6 Thinking                                    Context: 38k / 200k tokens (18%)",
    );
    assert_eq!(live_footer_idle.state, AgentState::Idle);
    assert_eq!(
        live_footer_idle
            .matched_rule
            .as_ref()
            .map(|rule| rule.id.as_str()),
        Some("live_prompt_footer")
    );
    assert!(live_footer_idle.visible_idle);

    let welcome_footer_idle = bundled_explain(
        Agent::Devin,
        "⠀⠀⠀⠀⠀⣴⣾⣶⡄⠀⠀⠀⠀\n⠀⣴⣾⣶⡾⠛⠿⠟⠃⣴⣾⣶⡄  Devin CLI\n⠀⠛⠿⠟⠃⣴⣾⣶⡾⠛⠿⠟⠃  v2026.5.26-8\n⠀⣤⣶⣦⡄⠻⢿⠿⢷⣤⣶⣦⡄\n⠀⠻⢿⠿⢷⣤⣶⣦⡄⠻⢿⠿⠃  Hybrid\n⠀⠀⠀⠀⠀⠻⢿⠿⠃⠀⠀⠀⠀\n\n───────────────────────────\n❭ Ask Devin to build\n  features, fix bugs, or\n  work on your code\n───────────────────────────\nClaude Opus Looking for\n4.6 Thinkingplan mode? /\n            plan",
    );
    assert_eq!(welcome_footer_idle.state, AgentState::Idle);
    assert_eq!(
        welcome_footer_idle
            .matched_rule
            .as_ref()
            .map(|rule| rule.id.as_str()),
        Some("welcome_prompt_footer")
    );
    assert!(welcome_footer_idle.visible_idle);

    let working = bundled_explain(
        Agent::Devin,
        "◔ Reading shell 91b655\n  │ Timeout: 35s\n\n⠀⡆ Running tools · 27s (esc to interrupt)\n─────────────────────────────────────────────────────\n❭ Guide Devin while it works",
    );
    assert_eq!(working.state, AgentState::Working);
    assert!(working.visible_working);

    let trust_prompt = bundled_explain(
        Agent::Devin,
        "Do you trust the authors of this directory?\nFor security, devin should not be run in directories\nwith untrusted content.\n❭ 1 Yes, trust /private/tmp/devin-hook-probe\n· 2 No, exit",
    );
    assert_eq!(trust_prompt.state, AgentState::Blocked);
    assert!(trust_prompt.visible_blocker);

    let permission_prompt = bundled_explain(
        Agent::Devin,
        "⏺ Running command\n  └ $ sleep 30\n\n❭ 1 Yes  (Approve once)\n· 2 Yes, allow `sleep` commands\n· 3 Yes, always allow `sleep` commands\n· 4 No\n↑↓ select · ↵ confirm · esc cancel",
    );
    assert_eq!(permission_prompt.state, AgentState::Blocked);
    assert!(permission_prompt.visible_blocker);
}

#[test]
fn muse_manifest_requires_complete_live_controls() {
    let working = explain(
        Agent::Muse,
        "⟩ hello\n\n◆ Working (0s · esc to interrupt)\n\n────────────────\n⟩\n────────────────\ngpt-5.4 · minimal · /workspace",
    );
    assert_eq!(working.state, AgentState::Working);
    assert!(working.visible_working);

    let picker = explain(
        Agent::Muse,
        "Which option should I use?\n\n› 1. Alpha\n  2. Beta\n\nEnter to select · ↑/↓ to move · Tab for an optional note · Esc to interrupt\n\n────────────────\n⟩\n────────────────\ngpt-5.4 · minimal · /workspace",
    );
    assert_eq!(picker.state, AgentState::Blocked);
    assert!(picker.visible_blocker);

    let command_approval = explain(
        Agent::Muse,
        "Would you like to run the following command?\n\n$ printf muse-safe-probe\n\n› 1. Allow this stage once (y)\n  2. Always allow in this workspace: printf muse-safe-probe ... (p)\n  3. Abort the entire command (esc)\n────────────────\ngpt-5.4 · minimal · /workspace",
    );
    assert_eq!(command_approval.state, AgentState::Blocked);
    assert!(command_approval.visible_blocker);

    let network_approval = explain(
        Agent::Muse,
        "network: example.com:443 https\nrequested by:\n$ curl -fsS https://example.com\n\n› 1. Yes, proceed (y)\n  2. Yes, don't ask again this session (p)  example.com:443 (https)\n  3. No, and tell Muse Code what to do differently (esc)\n────────────────\ngpt-5.4 · minimal · /workspace",
    );
    assert_eq!(network_approval.state, AgentState::Blocked);
    assert!(network_approval.visible_blocker);

    let menu = explain(
        Agent::Muse,
        "Theme\n\n⟩ Default (active)\n  Dynamic\n\n↑↓ move · enter save · esc go back",
    );
    assert_eq!(menu.state, AgentState::Unknown);
    assert!(menu.skip_state_update);
    assert!(!menu.visible_blocker);

    let ordinary_reply = explain(
        Agent::Muse,
        "⟩ say the phrase\n\n◆ Yes, proceed\n\n────────────────\n⟩\n────────────────\ngpt-5.4 · minimal · /workspace",
    );
    assert_eq!(ordinary_reply.state, AgentState::Idle);
    assert!(ordinary_reply.visible_idle);
}

#[test]
fn manifest_validation_rejects_unknown_fields_empty_rules_invalid_regions_and_regexes() {
    assert!(parse_manifest(
        r#"
id = "codex"

[[rules]]
id = "typo"
state = "working"
contain = ["Working"]
"#
    )
    .is_err());
    assert!(parse_manifest(
        r#"
id = "codex"

[[rules]]
id = "empty"
state = "working"
"#
    )
    .is_err());
    assert!(parse_manifest(
        r#"
id = "codex"

[[rules]]
id = "bad_region"
state = "working"
region = "after_last_promt_marker"
contains = ["Working"]
"#
    )
    .is_err());
    assert!(parse_manifest(
        r#"
id = "codex"

[[rules]]
id = "bad_regex"
state = "working"
regex = ["["]
"#
    )
    .is_err());
    assert!(parse_manifest(
        r#"
id = "codex"

[[rules]]
id = "bad_nested_regex"
state = "working"
any = [{ line_regex = ["["] }]
"#
    )
    .is_err());
}

#[test]
fn manifest_validation_keeps_skip_rules_neutral() {
    assert!(parse_manifest(
        r#"
id = "codex"

[[rules]]
id = "bad_skip_state"
state = "idle"
skip_state_update = true
contains = ["menu"]
"#
    )
    .is_err());
    assert!(parse_manifest(
        r#"
id = "codex"

[[rules]]
id = "bad_skip_visible"
state = "unknown"
skip_state_update = true
visible_blocker = true
contains = ["menu"]
"#
    )
    .is_err());
}

#[test]
fn manifest_validation_rejects_excessive_rule_count() {
    let mut manifest = String::from(
        r#"
id = "codex"
"#,
    );
    for index in 0..129 {
        manifest.push_str(&format!(
            r#"
[[rules]]
id = "rule_{index}"
state = "idle"
contains = ["ready"]
"#
        ));
    }
    assert!(parse_manifest(&manifest).is_err());
}

#[test]
fn manifest_validation_rejects_excessive_gate_depth() {
    let manifest = r#"
id = "codex"

[[rules]]
id = "deep"
state = "idle"
contains = ["ready"]
all = [
  { contains = ["1"], all = [
    { contains = ["2"], all = [
      { contains = ["3"], all = [
        { contains = ["4"], all = [
          { contains = ["5"], all = [
            { contains = ["6"], all = [
              { contains = ["7"], all = [
                { contains = ["8"], all = [
                  { contains = ["9"] },
                ] },
              ] },
            ] },
          ] },
        ] },
      ] },
    ] },
  ] },
]
"#;
    assert!(parse_manifest(manifest).is_err());
}

#[test]
fn manifest_validation_rejects_excessive_matchers() {
    let matchers = (0..33)
        .map(|index| format!(r#""m{index}""#))
        .collect::<Vec<_>>()
        .join(", ");
    let manifest = format!(
        r#"
id = "codex"

[[rules]]
id = "many"
state = "idle"
contains = [{matchers}]
"#
    );
    assert!(parse_manifest(&manifest).is_err());
}

#[test]
fn bottom_non_empty_lines_uses_bottom_occurrence_for_repeated_text() {
    let content = "marker\nold\n\nmiddle\nmarker\nnew\n";
    assert_eq!(
        region(
            DetectionInput {
                screen: content,
                osc_title: "",
                osc_progress: ""
            },
            "bottom_non_empty_lines(2)"
        ),
        "marker\nnew\n"
    );
}

#[test]
fn top_non_empty_lines_uses_top_occurrence_for_repeated_text() {
    let content = "\nmarker\nold\n\nmiddle\nmarker\nnew\n";
    assert_eq!(
        region(
            DetectionInput {
                screen: content,
                osc_title: "",
                osc_progress: ""
            },
            "top_non_empty_lines(2)"
        ),
        "\nmarker\nold\n"
    );
}

#[test]
fn top_non_empty_lines_requires_a_canonical_positive_bounded_count() {
    let name = "top_non_empty_lines";
    assert!(validate_region_name(&format!("{name}(1)")).is_ok());
    assert!(validate_region_name(&format!("{name}({})", u16::MAX)).is_ok());
    for count in ["0", "01", "+1", "65536", "999999999999999999999999"] {
        assert!(
            validate_region_name(&format!("{name}({count})")).is_err(),
            "{name} accepted invalid count {count}"
        );
    }
}

#[test]
fn top_non_empty_lines_requires_engine_three_when_declared() {
    let manifest = r#"
id = "codex"
version = "1"
min_engine_version = 2

[[rules]]
id = "background"
state = "working"
region = " top_non_empty_lines(1) "
contains = ["active"]
"#;
    assert!(parse_manifest(manifest).is_err());
}

// ---------------------------------------------------------------------------
// OSC rule tests — exercise the new osc_title / osc_progress regions against
// the bundled Claude and Codex manifests.
// ---------------------------------------------------------------------------

fn osc_explain(
    agent: Agent,
    screen: &str,
    osc_title: &str,
    osc_progress: &str,
) -> DetectionExplain {
    with_bundled_manifests("bundled-osc-explain", || {
        explain_with_input(
            agent,
            DetectionInput {
                screen,
                osc_title,
                osc_progress,
            },
        )
    })
}

// --- Claude OSC rules ---

#[test]
fn claude_osc_title_braille_prefix_is_working() {
    // "⠂" is U+2802, in the braille block U+2800-U+28FF
    let result = osc_explain(Agent::Claude, "", "⠂ project", "");
    assert_eq!(result.state, AgentState::Working);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_working")
    );
    assert!(result.visible_working);
}

#[test]
fn claude_osc_title_half_circle_frames_are_working() {
    for frame in ['◐', '◓', '◑', '◒'] {
        let title = format!("{frame} Initial conversation with Claude");
        let result = osc_explain(Agent::Claude, "", &title, "");
        assert_eq!(result.state, AgentState::Working, "frame {frame}");
        assert_eq!(
            result.matched_rule.as_ref().map(|rule| rule.id.as_str()),
            Some("osc_title_working"),
            "frame {frame}"
        );
        assert!(result.visible_working, "frame {frame}");
    }
}

#[test]
fn claude_osc_title_static_prefix_is_idle() {
    // "✳" is U+2733, static prefix when Claude is not working
    let result = osc_explain(Agent::Claude, "", "✳ Claude Code", "");
    assert_eq!(result.state, AgentState::Idle);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_idle")
    );
    assert!(result.visible_idle);
}

#[test]
fn claude_osc_progress_4_3_alone_does_not_force_working() {
    // Claude leaves progress stuck at 4;3 while waiting for permission, so
    // 4;3 must not be a working signal on its own. With no other evidence it
    // falls back to idle; blocked screen rules can win when present.
    let result = osc_explain(Agent::Claude, "", "", "4;3;");
    assert_eq!(result.state, AgentState::Idle);
    assert_eq!(
        result.fallback_reason.as_deref(),
        Some(DEFAULT_KNOWN_AGENT_IDLE_FALLBACK)
    );
    assert!(!result.visible_working);
}

#[test]
fn claude_blocker_screen_outranks_stale_osc_progress() {
    // Regression: progress 4;3 persists during permission prompts. The
    // blocked form on screen must win because no rule treats 4;3 as working.
    let blocker_screen =
        "──────────\n  1. Yes\n  2. No\n\nEnter to select · ↑/↓ to navigate · Esc to cancel\n";
    let result = osc_explain(Agent::Claude, blocker_screen, "✳ Task title", "4;3;");
    assert_eq!(result.state, AgentState::Blocked);
    assert!(result.visible_blocker);
}

#[test]
fn claude_wrapped_blocker_footer_is_blocked() {
    // Captured reporter panel at 48 columns: the footer wraps once between
    // "to" and "cancel" without inserting a blank line.
    let screen = "────────────────────────────────────────────────\n\
  4. Chat about this\n\n\
Enter to select · ↑/↓ to navigate · Esc to\n\
cancel\n";
    let result = osc_explain(Agent::Claude, screen, "", "");
    assert_eq!(result.state, AgentState::Blocked);
    assert_eq!(
        result.matched_rule.as_ref().map(|rule| rule.id.as_str()),
        Some("live_blocked_form")
    );
    assert!(result.visible_blocker);
}

#[test]
fn claude_selection_dialog_is_blocked_at_every_wrap_width() {
    let cases = [
        ("50 columns", "──────────────────────────────────────────────────\n  4. Chat about this\n\nEnter to select · ↑/↓ to navigate · Esc to cancel\n"),
        ("48 columns", "────────────────────────────────────────────────\n  4. Chat about this\n\nEnter to select · ↑/↓ to navigate · Esc to\ncancel\n"),
        ("40 columns", "────────────────────────────────────────\n  4. Chat about this\n\nEnter to select · ↑/↓ to navigate · Esc\nto cancel\n"),
        ("34 columns", "──────────────────────────────────\n  4. Chat about this\n\nEnter to select · ↑/↓ to\nnavigate · Esc to cancel\n"),
        ("24 columns", "────────────────────────\n  4. Chat about this\n\nEnter to\nselect · ↑/↓ to\nnavigate · Esc\nto cancel\n"),
        ("indented continuation", "────────────────────────\n  4. Chat about this\n\n  Enter to select · ↑/↓ to navigate · Esc to\n   cancel\n"),
        ("Enter to confirm wrap", "────────────────────────\n  Do you trust this folder?\n\nEnter to\nconfirm · Esc to cancel\n"),
    ];
    for (label, screen) in cases {
        let result = osc_explain(Agent::Claude, screen, "", "");
        assert_eq!(result.state, AgentState::Blocked, "{label}");
        assert_eq!(
            result.matched_rule.as_ref().map(|rule| rule.id.as_str()),
            Some("live_blocked_form"),
            "{label}"
        );
        assert!(result.visible_blocker, "{label}");
    }
}

#[test]
fn claude_background_shell_summary_is_working() {
    let result = bundled_explain(Agent::Claude, "⏵ Running background work · 2 shells ·\n");
    assert_eq!(result.state, AgentState::Working);
    assert_eq!(
        result.matched_rule.as_ref().map(|rule| rule.id.as_str()),
        Some("background_shell_working")
    );
    assert!(result.visible_working);
}

#[test]
fn claude_tasks_hint_reports_background_subagents_working() {
    let with_tasks = bundled_explain(
        Agent::Claude,
        "  -- INSERT -- ⏵⏵ bypass permissions on (shift+tab to cycle) · /tasks to see subagents · ← for agents",
    );
    assert_eq!(with_tasks.state, AgentState::Working);
    assert_eq!(
        with_tasks
            .matched_rule
            .as_ref()
            .map(|rule| rule.id.as_str()),
        Some("background_subagents_working")
    );

    let without_tasks = bundled_explain(
        Agent::Claude,
        "  -- INSERT -- ⏵⏵ bypass permissions on (shift+tab to cycle) · ← for agents",
    );
    assert_ne!(without_tasks.state, AgentState::Working);
    assert_ne!(
        without_tasks
            .matched_rule
            .as_ref()
            .map(|rule| rule.id.as_str()),
        Some("background_subagents_working")
    );
}

#[test]
fn claude_login_method_prompt_is_blocked() {
    let result = bundled_explain(
        Agent::Claude,
        "select login method:\n\
❯ 1. Claude account with subscription\n\
  2. Anthropic Console account\n\
  3. 3rd-party platform\n",
    );
    assert_eq!(result.state, AgentState::Blocked);
    assert_eq!(
        result.matched_rule.as_ref().map(|rule| rule.id.as_str()),
        Some("login_method_prompt")
    );
    assert!(result.visible_blocker);
}

#[test]
fn claude_wrap_tolerant_blocker_leaves_neighbouring_screens_alone() {
    for (label, screen, rule) in [
        (
            "streaming",
            "✽ Cooking… (6s · ↓ 174 tokens · thinking)\n",
            "live_turn_working",
        ),
        (
            "mode line",
            "⏵⏵ accept edits on · esc to interrupt\n",
            "live_turn_working",
        ),
        (
            "overlay",
            "  /btw\n  a note\n\nesc to close\n",
            "btw_overlay_working",
        ),
    ] {
        let result = osc_explain(Agent::Claude, screen, "", "");
        assert_eq!(result.state, AgentState::Working, "{label}");
        assert_eq!(
            result
                .matched_rule
                .as_ref()
                .map(|matched| matched.id.as_str()),
            Some(rule),
            "{label}"
        );
    }
    let transcript = osc_explain(
        Agent::Claude,
        "Showing detailed transcript · Ctrl+O to toggle\n",
        "",
        "",
    );
    assert_eq!(
        transcript
            .matched_rule
            .as_ref()
            .map(|rule| rule.id.as_str()),
        Some("transcript_viewer")
    );
    assert!(transcript.skip_state_update);
}

#[test]
fn claude_vim_mode_lines_are_live_turn_working_signals() {
    for screen in [
        "  -- INSERT -- ⏵⏵ bypass permissions on · esc to interrupt",
        "⏵⏵ bypass permissions on · esc to interrupt",
    ] {
        let result = bundled_explain(Agent::Claude, screen);
        assert_eq!(result.state, AgentState::Working, "{screen:?}");
        assert_eq!(
            result.matched_rule.as_ref().map(|rule| rule.id.as_str()),
            Some("live_turn_working"),
            "{screen:?}"
        );
        assert!(result.visible_working, "{screen:?}");
    }
}

#[test]
fn claude_blocker_footer_separator_spans_one_wrap_only() {
    for (label, screen) in [
        ("blank line", "────────────────────────\nEnter to confirm\n\nsomething esc to\n\ncancel\n"),
        ("NBSP", "────────────────────────\n  4. Chat about this\n\nEnter to select · ↑/↓ to navigate · Esc\u{a0}to\u{a0}cancel\n"),
        ("no whitespace", "────────────────────────\n  4. Chat about this\n\nEntertoselect · ↑/↓tonavigate · Esctocancel\n"),
    ] {
        let result = osc_explain(Agent::Claude, screen, "", "");
        assert_ne!(result.matched_rule.as_ref().map(|rule| rule.id.as_str()), Some("live_blocked_form"), "{label}");
    }
}

#[test]
fn claude_osc_progress_4_0_is_idle() {
    let result = osc_explain(Agent::Claude, "", "", "4;0;");
    assert_eq!(result.state, AgentState::Idle);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_progress_idle")
    );
}

#[test]
fn claude_blocker_screen_outranks_osc_idle_title() {
    // When the OSC title shows ✳ (idle) but the screen has a bash permission
    // prompt, the blocked rule at priority 1190 beats osc_title_idle at 250.
    let blocker_screen = "do you want to proceed?\n\
        bash command: rm -rf /tmp/test\n\
        ❯ 1. Yes\n   2. No\n\n\
        Esc to cancel · Tab to amend · ctrl+e to explain\n";
    let result = osc_explain(Agent::Claude, blocker_screen, "✳ Claude Code", "");
    assert_eq!(result.state, AgentState::Blocked);
    assert!(result.visible_blocker);
}

#[test]
fn claude_mcp_elicitation_is_blocked() {
    // Regression for issue #3283: an MCP elicitation dialog has Accept/Decline
    // controls and an "Esc to cancel" footer but no Enter hint, so no blocked
    // rule matched and the static OSC title reported idle.
    // Live capture uses curly quotes around the server name; the issue report
    // transcribed straight quotes. Both must classify as blocked.
    for screen in [
        "MCP server \u{201c}my-server\u{201d} requests your input\n\nGrant temporary access to the demo gateway for 15 minutes?\n\n\u{276f} Accept    Decline\n\nEsc to cancel \u{b7} \u{2191}/\u{2193} to navigate\n",
        "MCP server \"my-server\" requests your input\n\nserver-supplied message\n\n\u{276f} Accept    Decline\n\nEsc to cancel \u{b7} \u{2191}/\u{2193} to navigate\n",
    ] {
        let result = with_manifest_dirs("claude-mcp-elicitation", || {
            osc_explain(Agent::Claude, screen, "\u{2733} Claude Code", "")
        });
        assert_eq!(result.state, AgentState::Blocked, "{result:#?}");
        assert!(result.visible_blocker, "{result:#?}");
        assert_eq!(
            result.matched_rule.as_ref().map(|r| r.id.as_str()),
            Some("mcp_elicitation_prompt"),
            "{result:#?}"
        );
    }
}

#[test]
fn claude_empty_osc_empty_screen_is_idle_fallback() {
    // No OSC data, no matching screen rule → fallback idle (unchanged V3 behavior)
    let result = osc_explain(Agent::Claude, "", "", "");
    assert_eq!(result.state, AgentState::Idle);
    assert_eq!(
        result.fallback_reason.as_deref(),
        Some(DEFAULT_KNOWN_AGENT_IDLE_FALLBACK)
    );
    assert!(!result.visible_idle);
}

#[test]
fn claude_live_prompt_box_remains_a_positive_idle_observation() {
    let screen = "──────────\n❯\n──────────\n";
    let result = osc_explain(Agent::Claude, screen, "", "");

    assert_eq!(result.state, AgentState::Idle);
    assert_eq!(
        result.matched_rule.as_ref().map(|rule| rule.id.as_str()),
        Some("live_prompt_box")
    );
    assert!(result.visible_idle);
}

#[test]
fn claude_kimi_real_screen_fixtures_classify_idle_and_blocked() {
    let claude_idle = include_str!(
        "../../../tests/fixtures/agent-detection/claude-empty-prompt-ub1-wM-pJ-20260825.txt"
    );
    let kimi_through_claude = include_str!(
        "../../../tests/fixtures/agent-detection/kimi-through-claude-empty-prompt-ub1-wM-pK-20260825.txt"
    );
    let native_permission = include_str!(
        "../../../tests/fixtures/agent-detection/claude-native-bash-permission-20260825.txt"
    );
    let ask = include_str!(
        "../../../tests/fixtures/agent-detection/claude-native-ask-user-question-20260825.txt"
    );
    let narrow_asks = [
        (
            50,
            include_str!(
                "../../../tests/fixtures/agent-detection/claude-native-ask-user-question-width-50-20260825.txt"
            ),
        ),
        (
            48,
            include_str!(
                "../../../tests/fixtures/agent-detection/claude-native-ask-user-question-width-48-20260825.txt"
            ),
        ),
        (
            40,
            include_str!(
                "../../../tests/fixtures/agent-detection/claude-native-ask-user-question-width-40-20260825.txt"
            ),
        ),
        (
            34,
            include_str!(
                "../../../tests/fixtures/agent-detection/claude-native-ask-user-question-width-34-20260825.txt"
            ),
        ),
        (
            24,
            include_str!(
                "../../../tests/fixtures/agent-detection/claude-native-ask-user-question-width-24-20260825.txt"
            ),
        ),
    ];
    let narrow_layouts = include_str!(
        "../../../tests/fixtures/agent-detection/claude-native-ask-user-question-widths-20260825.layout.ndjson"
    )
    .lines()
    .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
    .collect::<Vec<_>>();
    let trust =
        include_str!("../../../tests/fixtures/agent-detection/claude-trust-folder-20260825.txt");
    let narrow_trust = include_str!(
        "../../../tests/fixtures/agent-detection/claude-trust-folder-narrow-20260825.txt"
    );
    let login =
        include_str!("../../../tests/fixtures/agent-detection/claude-login-method-20260825.txt");
    let quoted = include_str!(
        "../../../tests/fixtures/agent-detection/claude-quoted-blocker-live-prompt-20260825.txt"
    );
    let model_picker =
        include_str!("../../../tests/fixtures/agent-detection/claude-model-picker-20260825.txt");

    for screen in [claude_idle, kimi_through_claude] {
        let result = bundled_explain(Agent::Claude, screen);
        assert_eq!(result.state, AgentState::Idle);
        assert_eq!(
            result.matched_rule.as_ref().map(|rule| rule.id.as_str()),
            Some("live_prompt_box")
        );
        assert!(result.visible_idle);
    }

    for (label, screen) in [
        ("bash permission", native_permission),
        ("AskUserQuestion", ask),
        ("trust folder", trust),
        ("wrapped trust folder", narrow_trust),
        ("login method", login),
    ] {
        let result = osc_explain(Agent::Claude, screen, "", "4;3;");
        assert_eq!(result.state, AgentState::Blocked, "{label}");
        assert!(result.visible_blocker, "{label}");
        assert!(
            !screen.contains("closing-block") && !screen.contains("Gate "),
            "{label} must not depend on fleet tokens"
        );
    }

    for ((width, screen), layout) in narrow_asks.into_iter().zip(narrow_layouts) {
        assert_eq!(
            layout["result"]["layout"]["area"]["width"].as_u64(),
            Some(width as u64),
            "capture must retain exact physical pane width {width}"
        );
        assert_eq!(
            layout["result"]["layout"]["panes"][0]["rect"]["width"].as_u64(),
            Some(width as u64),
            "captured pane must match layout area at width {width}"
        );
        let top_border = screen.lines().find(|line| !line.is_empty()).unwrap();
        assert_eq!(
            top_border.chars().count(),
            width - 1,
            "Claude must paint its observed width-minus-one border at width {width}"
        );
        assert!(top_border.starts_with('╭') && top_border.ends_with('╮'));
        let result = osc_explain(Agent::Claude, screen, "", "4;3;");
        assert_eq!(result.state, AgentState::Blocked, "width {width}");
        assert!(result.visible_blocker, "width {width}");
        assert!(
            !screen.contains("closing-block") && !screen.contains("Gate "),
            "width {width} must not depend on fleet tokens"
        );
    }

    for (label, screen) in [("quoted history", quoted), ("model picker", model_picker)] {
        let result = bundled_explain(Agent::Claude, screen);
        assert!(!result.visible_blocker, "{label}");
        assert_ne!(result.state, AgentState::Blocked, "{label}");
    }
}

#[test]
fn fresh_hook_working_overrides_stale_native_claude_permission() {
    let native_permission = include_str!(
        "../../../tests/fixtures/agent-detection/claude-native-bash-permission-20260825.txt"
    );
    let result = osc_explain(Agent::Claude, native_permission, "⠋ Claude Code", "4;3;");
    assert_eq!(result.state, AgentState::Working);
    assert!(result.visible_working);
    assert_eq!(
        result.matched_rule.as_ref().map(|rule| rule.id.as_str()),
        Some("osc_title_working")
    );
}

#[test]
fn bundled_manifest_versions_cover_deployed_and_upstream_floors() {
    let claude: toml::Value = toml::from_str(include_str!("../manifests/claude.toml")).unwrap();
    let kimi: toml::Value = toml::from_str(include_str!("../manifests/kimi.toml")).unwrap();
    assert_eq!(claude["version"].as_str(), Some("2026.09.27.1"));
    assert!(kimi["version"]
        .as_str()
        .is_some_and(|version| version > "2026.06.10.1"));
    let claude_text = include_str!("../manifests/claude.toml");
    for working_rule in [
        "live_turn_working",
        "background_shell_working",
        "background_agents_working",
        "background_mcp_task_working",
    ] {
        assert!(
            claude_text.contains(working_rule),
            "missing deployed working rule {working_rule}"
        );
    }
}

// --- Codex OSC rules ---

#[test]
fn codex_osc_title_braille_spinner_is_working() {
    // "⠋" is U+280B, in the braille block
    let result = osc_explain(Agent::Codex, "", "⠋ llm-proxy", "");
    assert_eq!(result.state, AgentState::Working);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_working")
    );
    assert!(result.visible_working);
}

#[test]
fn codex_osc_title_action_required_is_blocked() {
    let result = osc_explain(Agent::Codex, "", "[ . ] Action Required | llm-proxy", "");
    assert_eq!(result.state, AgentState::Blocked);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_blocked")
    );
    assert!(result.visible_blocker);
}

#[test]
fn codex_osc_title_plain_is_idle() {
    let result = osc_explain(Agent::Codex, "", "llm-proxy", "");
    assert_eq!(result.state, AgentState::Idle);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_idle")
    );
    assert!(result.visible_idle);
}

#[test]
fn codex_trust_directory_requires_live_top_region() {
    let screen = "> You are in C:\\Users\\user\\project\n\n\
        Do you trust the contents of this\n\
        directory? Working with untrusted\n\
        contents comes with higher risk of\n\
        prompt injection. Trusting the\n\
        directory allows project-local config,\n\
        hooks, and exec policies to load.\n\n\
        › 1. Yes, continue\n\
          2. No, quit\n\n\
        Press enter to continue\n";
    let result = osc_explain(Agent::Codex, screen, "project", "");

    assert_eq!(result.state, AgentState::Blocked);
    assert_eq!(
        result.matched_rule.as_ref().map(|rule| rule.id.as_str()),
        Some("trust_directory")
    );
    assert!(result.visible_blocker);

    let transcript = "› > You are in C:\\Users\\user\\project\n\n\
        Do you trust the contents of this\n\
        directory? Working with untrusted contents comes with higher risk.\n";
    let result = osc_explain(Agent::Codex, transcript, "project", "");

    assert_eq!(result.state, AgentState::Idle);
    assert_ne!(
        result.matched_rule.as_ref().map(|rule| rule.id.as_str()),
        Some("trust_directory")
    );
    assert!(!result.visible_blocker);
}

#[test]
fn codex_background_terminal_screen_does_not_override_osc_idle() {
    // Background terminal tasks can be long-lived helpers such as dev servers.
    // They should not make Codex look busy once the foreground turn is idle.
    let screen = "background terminal running · /ps to view · /stop to close\n";
    let result = osc_explain(Agent::Codex, screen, "llm-proxy", "");
    assert_eq!(result.state, AgentState::Idle);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_idle")
    );
    assert!(result.visible_idle);
}

#[test]
fn codex_screen_working_fallback_handles_static_osc_title_and_progress_decorations() {
    for progress_line in [
        "◦ Working (1m 16s • esc to interrupt) · 1 background…",
        "Working (1m 16s • esc to interrupt)",
    ] {
        let screen = format!(
            "• I’ll run it and wait for completion.\n\n{progress_line}\n\n\
             › Use /skills to list available skills\n\n\
             gpt-5.6-sol default · /work\n\
             footer detail\n"
        );
        let result = osc_explain(Agent::Codex, &screen, "project", "");

        assert_eq!(result.state, AgentState::Working, "{progress_line}");
        assert_eq!(
            result.matched_rule.as_ref().map(|r| r.id.as_str()),
            Some("screen_working_fallback"),
            "{progress_line}"
        );
        assert_eq!(
            result
                .matched_rule
                .as_ref()
                .map(|rule| rule.region.as_str()),
            Some("before_current_prompt_marker"),
            "{progress_line}"
        );
        assert!(result.visible_working, "{progress_line}");
    }
}

/// Verbatim from a codex-cli 0.147.0 pane read back through `agent.read` while
/// it ran a shell command. Note what is *not* here: no bullet, no spinner, and
/// an OSC title that never leaves the cwd. This screen is the reason the rule
/// cannot anchor itself to a block marker.
const CODEX_TOOL_CALL_SCREEN: &str = "\
› Run exactly this shell command and nothing else, then report the exit code: sleep 45

Working (19s • esc to interrupt) · 1 background terminal running · /ps to view

› Write tests for @filename

  gpt-5.6-luna medium · Context 97% left · 8.69K used · ~/workspaces/personal
";

/// The same pane a few seconds later, streaming its answer. Codex removes the
/// progress line while it writes, so nothing on screen separates this from a
/// finished turn -- only the bytes still arriving do.
const CODEX_STREAMING_SCREEN: &str = "\
› Explain in detail, step by step, how the Rust borrow checker works.

• Rust's borrow checker enforces one central rule:

  let value = String::from(\"hello\");
  let reference = &value;

› Use /skills to list available skills

  gpt-5.6-luna medium · Context 100% left · ~/workspaces/personal
";

#[test]
fn a_codex_pane_running_a_tool_is_working_even_though_it_printed_no_block_marker() {
    let result = osc_explain(Agent::Codex, CODEX_TOOL_CALL_SCREEN, "personal", "");

    assert_eq!(result.state, AgentState::Working);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("screen_working_fallback")
    );
    assert!(result.visible_working);
}

#[test]
fn a_streaming_codex_pane_shows_no_working_evidence_on_screen_at_all() {
    let result = osc_explain(Agent::Codex, CODEX_STREAMING_SCREEN, "personal", "");

    // The screen has no working evidence; the static OSC title is explicit
    // idle evidence in the upstream manifest.
    assert_eq!(result.state, AgentState::Idle);
    assert!(!result.visible_working);
    assert!(result.visible_idle);
}

#[test]
fn codex_osc_working_remains_preferred_over_screen_fallback() {
    let screen = "• Working (4s • esc to interrupt)\n\n\
        › Use /skills to list available skills\n\n\
        gpt-5.6-sol default · /work\n";
    let result = osc_explain(Agent::Codex, screen, "⠸ project", "");

    assert_eq!(result.state, AgentState::Working);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_working")
    );
    assert!(result.visible_working);
}

#[test]
fn codex_screen_blocker_outranks_working_fallback() {
    let screen = "• Working (4s • esc to interrupt)\n\
        › 1. Yes, proceed\n\
        Press enter to confirm or esc to cancel\n";
    let result = osc_explain(Agent::Codex, screen, "project", "");

    assert_eq!(result.state, AgentState::Blocked);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("live_strong_blocker")
    );
    assert!(result.visible_blocker);
    assert!(!result.visible_working);
}

#[test]
fn codex_weak_blocker_without_current_prompt_is_blocked() {
    let result = osc_explain(
        Agent::Codex,
        "do you want to continue? [y/n]\n",
        "project",
        "",
    );

    assert_eq!(result.state, AgentState::Blocked);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("weak_blocker")
    );
}

#[test]
fn codex_current_prompt_keeps_weak_text_from_overriding_working_fallback() {
    let screen = "• Working (4s • esc to interrupt)\n\
        do you want to continue? [y/n]\n\
        › Use /skills to list available skills\n";
    let result = osc_explain(Agent::Codex, screen, "project", "");

    assert_eq!(result.state, AgentState::Working);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("screen_working_fallback")
    );
    assert!(result.visible_working);
}

#[test]
fn codex_weak_blocker_ignores_finished_response_above_current_prompt() {
    let screen = "• The `wt rm` transcript now shows [y/N] / esc, matching the real prompt.\n\n\
        ─ Worked for 4m 59s ─\n\n\
        › Ask Codex to do anything\n";
    let result = osc_explain(Agent::Codex, screen, "project", "");

    assert_eq!(result.state, AgentState::Idle);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_idle")
    );
}

#[test]
fn codex_weak_blocker_ignores_wrapped_current_prompt_text() {
    let screen = "› Explain why this prompt wraps before quoting the confirmation text\n\
          [y/N] / esc and whether the docs should include it\n\n\
          gpt-5.6-sol default · /work\n";
    let result = osc_explain(Agent::Codex, screen, "project", "");

    assert_eq!(result.state, AgentState::Idle);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_idle")
    );
}

#[test]
fn codex_transcript_viewer_outranks_working_fallback() {
    let screen = "• Working (4s • esc to interrupt)\n\
        › transcript\n\
        ↑/↓ to scroll · pgup/pgdn to move · home/end to jump · q to quit · esc to edit prev\n";
    let result = osc_explain(Agent::Codex, screen, "project", "");

    assert_eq!(result.state, AgentState::Unknown);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("transcript_viewer")
    );
    assert!(result.skip_state_update);
    assert!(!result.visible_working);
}

#[test]
fn codex_osc_working_beats_weak_blocker_screen() {
    // A stale [y/n] on screen triggers weak_blocker at priority 600, but an
    // active braille spinner in the OSC title is priority 1050 — OSC wins.
    let screen = "do you want to continue? [y/n]\n";
    let result = osc_explain(Agent::Codex, screen, "⠋ llm-proxy", "");
    assert_eq!(result.state, AgentState::Working);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_working")
    );
}

#[test]
fn claude_usage_limit_screen_reads_as_a_usage_blocker() {
    // The plan-limit banner sits right above the live prompt box, so the idle
    // rule (950) also matches; the usage rule at 960 has to win.
    let screen = "\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\n\
        You've hit your 5-hour limit resets 3pm\n\
        /upgrade to increase your usage limit.\n\
        \u{256d}\u{2500}\u{2500}\u{256e}\n\
        \u{2502} \u{276f}   \u{2502}\n\
        \u{2570}\u{2500}\u{2500}\u{256f}\n";
    let result = osc_explain(Agent::Claude, screen, "", "");

    assert_eq!(result.state, AgentState::Blocked);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("usage_limit_reached")
    );
    assert!(result.visible_blocker);
    assert!(result.usage_limited);
}

#[test]
fn claude_usage_limit_clears_as_soon_as_the_agent_works() {
    // Live screen only: the banner may still be on screen, but a working OSC
    // title (1100) outranks it, so nothing latches.
    let screen = "You've hit your 5-hour limit resets 3pm\n\
        /upgrade to increase your usage limit.\n";
    let result = osc_explain(Agent::Claude, screen, "\u{2807} scalable", "");

    assert_eq!(result.state, AgentState::Working);
    assert!(!result.usage_limited);
}

#[test]
fn codex_usage_limit_screen_reads_as_a_usage_blocker() {
    let screen = "You've hit your usage limit for gpt-5.6-sol.\n\
        Try again at 4:15pm.\n";
    let result = osc_explain(Agent::Codex, screen, "", "");

    assert_eq!(result.state, AgentState::Blocked);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("usage_limit_reached")
    );
    assert!(result.visible_blocker);
    assert!(result.usage_limited);
}

#[test]
fn codex_usage_limit_clears_as_soon_as_the_agent_works() {
    let screen = "You've hit your usage limit for gpt-5.6-sol.\n\
        Try again later.\n";
    let result = osc_explain(Agent::Codex, screen, "\u{2839} llm-proxy", "");

    assert_eq!(result.state, AgentState::Working);
    assert!(!result.usage_limited);
}

#[test]
fn an_ordinary_claude_question_is_not_a_usage_blocker() {
    let screen = "\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\n\
        Do you want to proceed?\n\
        \u{276f} 1. Yes\n\
        enter to select \u{00b7} esc to cancel \u{00b7} arrow keys to navigate\n";
    let result = osc_explain(Agent::Claude, screen, "", "");

    assert_eq!(result.state, AgentState::Blocked);
    assert!(result.visible_blocker);
    assert!(!result.usage_limited);
}

#[test]
fn claude_auth_failure_over_live_prompt_is_blocked() {
    let fixture =
        include_str!("../../../tests/fixtures/agent-detection/claude-auth-failure-20260831.txt");
    for banner in [
        "Invalid API key · Please run /login",
        "OAuth token expired · Please run /login",
        "OAuth token revoked · Please run /login",
        "Your session has expired. Please run /login to sign in again.",
        "Credit balance is too low",
        "Your account does not have access to Claude. Please login again or contact your administrator.",
    ] {
        let screen = fixture_with_banner(fixture, banner);
        let result = bundled_explain(Agent::Claude, &screen);

        assert_eq!(result.state, AgentState::Blocked, "{banner}");
        assert_eq!(
            result.matched_rule.as_ref().map(|rule| rule.id.as_str()),
            Some("auth_failure"),
            "{banner}"
        );
        assert!(result.visible_blocker, "{banner}");
        assert!(!result.usage_limited, "{banner}");
    }
}

#[test]
fn claude_api_error_over_live_prompt_is_blocked() {
    let fixture =
        include_str!("../../../tests/fixtures/agent-detection/claude-api-error-20260831.txt");
    for banner in [
        "API Error: 500",
        "API Error (Connection error)",
        "Request timed out",
        "API Error: fetch failed",
        "API Error: 529 {\"type\":\"overloaded_error\"}",
    ] {
        let screen = fixture_with_banner(fixture, banner);
        let result = bundled_explain(Agent::Claude, &screen);

        assert_eq!(result.state, AgentState::Blocked, "{banner}");
        assert_eq!(
            result.matched_rule.as_ref().map(|rule| rule.id.as_str()),
            Some("api_error"),
            "{banner}"
        );
        assert!(result.visible_blocker, "{banner}");
        assert!(!result.usage_limited, "{banner}");
    }
}

#[test]
fn claude_working_spinner_clears_a_visible_api_error() {
    let screen =
        include_str!("../../../tests/fixtures/agent-detection/claude-api-error-20260831.txt");
    let result = osc_explain(Agent::Claude, screen, "⠋ Claude Code", "");

    assert_eq!(result.state, AgentState::Working);
    assert_eq!(
        result.matched_rule.as_ref().map(|rule| rule.id.as_str()),
        Some("osc_title_working")
    );
    assert!(!result.visible_blocker);
}

#[test]
fn codex_auth_failure_over_live_prompt_is_blocked() {
    let fixture =
        include_str!("../../../tests/fixtures/agent-detection/codex-auth-failure-20260831.txt");
    for banner in [
        "■ Your access token could not be refreshed because your refresh token has expired. Please log out and sign in again.",
        "■ Your access token could not be refreshed because your refresh token was already used. Please log out and sign in again.",
        "■ Your access token could not be refreshed because your refresh token was revoked. Please log out and sign in again.",
        "■ Your access token could not be refreshed. Please log out and sign in again.",
        "■ Your access token could not be refreshed because you have since logged out or signed in to another account. Please sign in again.",
        "■ Quota exceeded. Check your plan and billing details.",
    ] {
        let screen = fixture_with_banner(fixture, banner);
        let result = bundled_explain(Agent::Codex, &screen);

        assert_eq!(result.state, AgentState::Blocked, "{banner}");
        assert_eq!(
            result.matched_rule.as_ref().map(|rule| rule.id.as_str()),
            Some("auth_failure"),
            "{banner}"
        );
        assert!(result.visible_blocker, "{banner}");
        assert!(!result.usage_limited, "{banner}");
    }
}

#[test]
fn codex_api_error_over_live_prompt_is_blocked() {
    let fixture =
        include_str!("../../../tests/fixtures/agent-detection/codex-api-error-20260831.txt");
    for banner in [
        "■ request timed out",
        "■ Connection failed: connection reset by peer",
        "■ Error while reading the server response: connection closed",
        "■ We're currently experiencing high demand, which may cause temporary errors.",
        "⚠ Selected model is at capacity. Please try a different model.",
    ] {
        let screen = fixture_with_banner(fixture, banner);
        let result = bundled_explain(Agent::Codex, &screen);

        assert_eq!(result.state, AgentState::Blocked, "{banner}");
        assert_eq!(
            result.matched_rule.as_ref().map(|rule| rule.id.as_str()),
            Some("api_error"),
            "{banner}"
        );
        assert!(result.visible_blocker, "{banner}");
        assert!(!result.usage_limited, "{banner}");
    }
}

#[test]
fn codex_working_spinner_clears_a_visible_api_error() {
    let error =
        include_str!("../../../tests/fixtures/agent-detection/codex-api-error-20260831.txt");
    let screen = error.replacen("\n›\n", "\nWorking (1s • esc to interrupt)\n\n›\n", 1);
    let result = bundled_explain(Agent::Codex, &screen);

    assert_eq!(result.state, AgentState::Working);
    assert_eq!(
        result.matched_rule.as_ref().map(|rule| rule.id.as_str()),
        Some("screen_working_fallback")
    );
    assert!(!result.visible_blocker);
}

#[test]
fn codex_v0158_question_and_confirmation_snapshots_are_blocked() {
    let fixtures = [
        (
            "startup hooks review",
            include_str!("../../../tests/fixtures/agent-detection/codex-startup-hooks-review-20260928.txt"),
            "startup_hooks_review",
        ),
        (
            "live startup hooks review",
            include_str!("../../../tests/fixtures/agent-detection/codex-startup-hooks-review-live-20260928.txt"),
            "startup_hooks_review",
        ),
        (
            "startup hooks review with option 2 selected",
            include_str!("../../../tests/fixtures/agent-detection/codex-startup-hooks-review-selection-moved-20260928.txt"),
            "startup_hooks_review",
        ),
        (
            "startup hooks review with trust error",
            include_str!("../../../tests/fixtures/agent-detection/codex-startup-hooks-review-trust-error-20260928.txt"),
            "startup_hooks_review",
        ),
        (
            "folder access",
            include_str!("../../../tests/fixtures/agent-detection/codex-folder-access-20260928.txt"),
            "folder_access_prompt",
        ),
        (
            "live folder access",
            include_str!("../../../tests/fixtures/agent-detection/codex-folder-access-live-20260928.txt"),
            "folder_access_prompt",
        ),
        (
            "folder access with option 2 selected",
            include_str!("../../../tests/fixtures/agent-detection/codex-folder-access-selection-moved-20260928.txt"),
            "folder_access_prompt",
        ),
        (
            "folder access with trust error",
            include_str!("../../../tests/fixtures/agent-detection/codex-folder-access-trust-error-20260928.txt"),
            "folder_access_prompt",
        ),
        (
            "folder access existing task choice",
            include_str!("../../../tests/fixtures/agent-detection/codex-folder-access-existing-task-20260928.txt"),
            "folder_access_prompt",
        ),
        (
            "folder access restricted folder choice",
            include_str!("../../../tests/fixtures/agent-detection/codex-folder-access-restricted-20260928.txt"),
            "folder_access_prompt",
        ),
        (
            "browser sign in",
            include_str!("../../../tests/fixtures/agent-detection/codex-browser-sign-in-20260928.txt"),
            "browser_sign_in",
        ),
        (
            "narrow browser sign in",
            include_str!("../../../tests/fixtures/agent-detection/codex-browser-sign-in-narrow-20260928.txt"),
            "browser_sign_in",
        ),
        (
            "remapped request_user_input submit",
            include_str!("../../../tests/fixtures/agent-detection/codex-request-user-input-remapped-submit-20260928.txt"),
            "request_user_input",
        ),
        (
            "unanswered request_user_input confirmation",
            include_str!("../../../tests/fixtures/agent-detection/codex-request-user-input-unanswered-confirmation-20260928.txt"),
            "live_strong_blocker",
        ),
        (
            "wrapped approval footer",
            include_str!("../../../tests/fixtures/agent-detection/codex-approval-wrapped-footer-20260928.txt"),
            "live_strong_blocker",
        ),
        (
            "MCP elicitation approval",
            include_str!("../../../tests/fixtures/agent-detection/codex-mcp-elicitation-approval-20260928.txt"),
            "mcp_server_elicitation",
        ),
        (
            "MCP elicitation text input",
            include_str!("../../../tests/fixtures/agent-detection/codex-mcp-elicitation-text-20260928.txt"),
            "mcp_server_elicitation",
        ),
        (
            "model migration",
            include_str!("../../../tests/fixtures/agent-detection/codex-model-migration-20260928.txt"),
            "model_migration",
        ),
        (
            "narrow model migration choice",
            include_str!("../../../tests/fixtures/agent-detection/codex-model-migration-narrow-20260928.txt"),
            "model_migration",
        ),
        (
            "update prompt",
            include_str!("../../../tests/fixtures/agent-detection/codex-update-prompt-20260928.txt"),
            "update_prompt_modal",
        ),
        (
            "narrow update prompt",
            include_str!("../../../tests/fixtures/agent-detection/codex-update-prompt-narrow-20260928.txt"),
            "update_prompt_modal",
        ),
        (
            "app link elicitation",
            include_str!("../../../tests/fixtures/agent-detection/codex-app-link-elicitation-20260928.txt"),
            "interactive_choice",
        ),
        (
            "app link elicitation confirmation",
            include_str!("../../../tests/fixtures/agent-detection/codex-app-link-elicitation-confirmation-20260928.txt"),
            "interactive_choice",
        ),
        (
            "external agent setup import",
            include_str!("../../../tests/fixtures/agent-detection/codex-external-agent-import-20260928.txt"),
            "interactive_choice",
        ),
        (
            "full access confirmation",
            include_str!("../../../tests/fixtures/agent-detection/codex-full-access-confirmation-20260928.txt"),
            "interactive_choice",
        ),
        (
            "memory reset confirmation",
            include_str!("../../../tests/fixtures/agent-detection/codex-memory-reset-confirmation-20260928.txt"),
            "interactive_choice",
        ),
        (
            "rate limit reset confirmation",
            include_str!("../../../tests/fixtures/agent-detection/codex-rate-limit-reset-confirmation-20260928.txt"),
            "interactive_choice",
        ),
        (
            "delete session confirmation",
            include_str!("../../../tests/fixtures/agent-detection/codex-delete-session-confirmation-20260928.txt"),
            "interactive_choice",
        ),
        (
            "safety retry confirmation",
            include_str!("../../../tests/fixtures/agent-detection/codex-safety-retry-confirmation-20260928.txt"),
            "safety_retry_confirmation",
        ),
        (
            "safety retry confirmation with option 2 selected",
            include_str!("../../../tests/fixtures/agent-detection/codex-safety-retry-confirmation-selection-moved-20260928.txt"),
            "safety_retry_confirmation",
        ),
    ];

    for (name, screen, expected_rule) in fixtures {
        let result = bundled_explain(Agent::Codex, screen);

        assert_eq!(result.state, AgentState::Blocked, "{name}");
        assert!(result.visible_blocker, "{name}");
        assert_eq!(
            result
                .matched_rule
                .as_ref()
                .map(|matched| matched.id.as_str()),
            Some(expected_rule),
            "{name}"
        );
    }
}

#[test]
fn codex_question_prompts_above_idle_composer_are_not_blocked() {
    let fixtures = [
        (
            "startup hooks review",
            include_str!("../../../tests/fixtures/agent-detection/codex-startup-hooks-review-20260928.txt"),
        ),
        (
            "browser sign in",
            include_str!("../../../tests/fixtures/agent-detection/codex-browser-sign-in-20260928.txt"),
        ),
        (
            "request user input",
            include_str!("../../../tests/fixtures/agent-detection/codex-request-user-input-remapped-submit-20260928.txt"),
        ),
        (
            "MCP server elicitation",
            include_str!("../../../tests/fixtures/agent-detection/codex-mcp-elicitation-approval-20260928.txt"),
        ),
        (
            "safety retry confirmation",
            include_str!("../../../tests/fixtures/agent-detection/codex-safety-retry-confirmation-20260928.txt"),
        ),
    ];

    for (name, prompt) in fixtures {
        let screen = format!("{prompt}\n› follow-up text\n\n  GPT-6-Luna default · /repo");
        let result = bundled_explain(Agent::Codex, &screen);

        assert_ne!(result.state, AgentState::Blocked, "{name}");
    }

    let narrow_browser_prompt = include_str!(
        "../../../tests/fixtures/agent-detection/codex-browser-sign-in-narrow-20260928.txt"
    );
    let screen = format!("{narrow_browser_prompt}\n› \n\n  GPT-6-Luna default · /repo");
    let result = bundled_explain(Agent::Codex, &screen);

    assert_ne!(result.state, AgentState::Blocked, "narrow browser sign in");
}

#[test]
fn codex_quoted_hook_prompt_above_idle_composer_is_not_blocked() {
    let screen = "Earlier output quoted ‘Hooks need review’ and ‘Trust all and continue’.\n› What does that prompt mean?\n";
    let result = bundled_explain(Agent::Codex, screen);

    assert_ne!(result.state, AgentState::Blocked);
    assert!(!result.visible_blocker);
}

#[test]
fn ordinary_claude_permission_prompt_keeps_its_existing_rule() {
    let screen = include_str!(
        "../../../tests/fixtures/agent-detection/claude-native-bash-permission-20260825.txt"
    );
    let result = bundled_explain(Agent::Claude, screen);

    assert_eq!(result.state, AgentState::Blocked);
    assert_eq!(
        result.matched_rule.as_ref().map(|rule| rule.id.as_str()),
        Some("bash_permission_prompt")
    );
    assert!(result.visible_blocker);
    assert!(!result.usage_limited);
}

fn assert_claude_visible_blocker(result: &DetectionExplain, rule: &str, label: &str) {
    assert_eq!(result.state, AgentState::Blocked, "{label}");
    assert_eq!(
        result.matched_rule.as_ref().map(|rule| rule.id.as_str()),
        Some(rule),
        "{label}"
    );
    assert!(result.visible_blocker, "{label}");
    assert!(!result.usage_limited, "{label}");
}

#[test]
fn claude_plan_approval_dialog_is_a_visible_blocker() {
    let screen = include_str!(
        "../../../tests/fixtures/agent-detection/claude-plan-approval-ub1-20260927.txt"
    );

    let result = bundled_explain(Agent::Claude, screen);
    assert_claude_visible_blocker(&result, "plan_approval_prompt", "no OSC");

    // Claude keeps the idle title glyph while the dialog waits for an answer.
    let result = osc_explain(Agent::Claude, screen, "✳ Create probe file", "");
    assert_claude_visible_blocker(&result, "plan_approval_prompt", "idle OSC title");
}

#[test]
fn claude_file_write_permission_prompt_is_a_visible_blocker() {
    let screen = include_str!(
        "../../../tests/fixtures/agent-detection/claude-write-permission-ub1-20260927.txt"
    );

    let result = bundled_explain(Agent::Claude, screen);
    assert_claude_visible_blocker(&result, "file_permission_prompt", "no OSC");

    let result = osc_explain(Agent::Claude, screen, "✳ Create probe file", "");
    assert_claude_visible_blocker(&result, "file_permission_prompt", "idle OSC title");
}

#[test]
fn claude_wrapped_file_edit_permission_prompt_is_a_visible_blocker() {
    let rule = "─".repeat(80);
    let screen = format!(
        "{rule}\n Edit file\n probe.txt\n{rule}\n Do you want to make this\n edit to probe.txt?\n ❯ 1. Yes\n   2. Yes, and switch to accept edits (auto-approve file edits and common\n      file commands) for this session (shift+tab)\n   3. No\n\n Esc to cancel · Tab to amend\n"
    );

    let result = bundled_explain(Agent::Claude, &screen);
    assert_claude_visible_blocker(&result, "file_permission_prompt", "wrapped edit");
}

#[test]
fn current_claude_bash_permission_panel_keeps_its_existing_rule() {
    let screen = include_str!(
        "../../../tests/fixtures/agent-detection/claude-native-bash-permission-ub1-20260927.txt"
    );

    let result = bundled_explain(Agent::Claude, screen);
    assert_claude_visible_blocker(&result, "bash_permission_prompt", "no OSC");

    let result = osc_explain(Agent::Claude, screen, "✳ Create probe file", "");
    assert_claude_visible_blocker(&result, "bash_permission_prompt", "idle OSC title");
}

#[test]
fn quoted_plan_and_file_prompts_above_a_live_prompt_stay_idle() {
    let rule = "─".repeat(80);
    let screen = format!(
        "❯ Quote the two Claude dialogs back to me.\n\n⏺ The plan dialog says \"Claude has written up a plan and is ready to execute. Would you like\n  to proceed?\" with 1. Yes, and use auto mode, 2. Yes, manually approve edits and\n  3. Tell Claude what to change. The write dialog asks \"Do you want to create probe.txt?\"\n  with 1. Yes, 2. Yes, and switch to accept edits, 3. No and Esc to cancel.\n\n{rule}\n❯\n{rule}\n  -- INSERT -- ⏸ manual mode on · ← 13 agents /rc\n"
    );

    for (label, osc_title) in [("no OSC", ""), ("idle OSC title", "✳ Create probe file")] {
        let result = osc_explain(Agent::Claude, &screen, osc_title, "");
        assert_eq!(result.state, AgentState::Idle, "{label}");
        assert!(!result.visible_blocker, "{label}");
    }
}
