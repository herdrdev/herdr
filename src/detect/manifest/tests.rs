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
    let _guard = crate::config::test_config_env_lock().lock().unwrap();
    let old_config = std::env::var_os("XDG_CONFIG_HOME");
    let old_state = std::env::var_os("XDG_STATE_HOME");
    let base = std::env::temp_dir().join(format!(
        "herdr-manifest-loader-{name}-{}",
        std::process::id()
    ));
    let config_dir = base.join("config");
    let state_dir = base.join("state");
    let _ = std::fs::remove_dir_all(&base);
    std::env::set_var("XDG_CONFIG_HOME", &config_dir);
    std::env::set_var("XDG_STATE_HOME", &state_dir);
    reload_manifests();
    let result = f();
    match old_config {
        Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
        None => std::env::remove_var("XDG_CONFIG_HOME"),
    }
    match old_state {
        Some(value) => std::env::set_var("XDG_STATE_HOME", value),
        None => std::env::remove_var("XDG_STATE_HOME"),
    }
    reload_manifests();
    let _ = std::fs::remove_dir_all(&base);
    result
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
fn submission_verification_reports_the_effective_manifest() {
    with_manifest_dirs("composer-support", || {
        let summaries = manifest_summaries();
        assert!(
            summaries
                .iter()
                .find(|summary| summary.agent == Agent::Claude)
                .unwrap()
                .submission_verification_supported
        );
        assert!(
            !summaries
                .iter()
                .find(|summary| summary.agent == Agent::Codex)
                .unwrap()
                .submission_verification_supported
        );
        assert!(submission_verification_supported(Agent::Claude));
        assert!(!submission_verification_supported(Agent::Codex));

        write_local_codex(&format!(
            "{}\n[composer]\nregion = \"prompt_box_body\"\n",
            local_manifest("working", "active-marker")
        ));
        assert!(
            manifest_summaries()
                .iter()
                .find(|summary| summary.agent == Agent::Codex)
                .unwrap()
                .submission_verification_supported
        );
        assert!(submission_verification_supported(Agent::Codex));
    });
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

// A synthetic terminal protocol: live controls may precede a named divider
// and an optional panel, but a newer prompt/output makes them historical.
#[test]
fn end_anchored_footer_gates_allow_only_auxiliary_tail() {
    with_manifest_dirs("footer-tail", || {
        write_local_codex(&rules_manifest(
            r#"
[[rules]]
id = "live_controls"
state = "blocked"
region = "whole_recent"
regex = ['(?i)(?:^|\n)controls:\s+(?:confirm-marker|select-marker\s+navigate-marker)\s+cancel-marker[ \t]*\n---[^\n]*(?:\npanel: \d+(?:\nrow: [^\n]+)+(?:\n… \+\d+ pending)?)?(?:\n[ \t]*)*\z']
"#,
        ));
        let footer = "controls: select-marker navigate-marker cancel-marker";
        for tail in [
            "--- name",
            "--- name\npanel: 5\nrow: active\nrow: pending\nrow: pending\nrow: pending\nrow: done\n",
            "--- name\npanel: 5\nrow: active\nrow: done\n… +3 pending",
        ] {
            let live = format!("old output\n{footer}\n{tail}");
            assert_eq!(explain(Agent::Codex, &live).state, AgentState::Blocked);
            for later in [
                "new output",
                "prompt:",
                "--- next\nprompt:",
                "prompt: continue…",
                "working: processing…",
                "…ordinary output",
                "finished…",
                "… +2 arbitrary words",
            ] {
                assert_eq!(
                    explain(Agent::Codex, &format!("{live}\n{later}")).state,
                    AgentState::Idle
                );
            }
        }
        for tail in [
            "--- name\n… +2 pending",
            "--- name\nrow: active",
            "--- name\npanel: 5",
            "--- name\npanel: 5\nrow: active\n  working: processing…",
            "--- name\npanel: 5\nrow: active\n  prompt: continue…",
        ] {
            assert_eq!(
                explain(Agent::Codex, &format!("{footer}\n{tail}")).state,
                AgentState::Idle
            );
        }
        for controls in [
            "controls: confirm-marker cancel-marker",
            "controls: select-marker\nnavigate-marker cancel-marker",
            "controls:\nselect-marker\nnavigate-marker\ncancel-marker",
        ] {
            assert_eq!(
                explain(Agent::Codex, &format!("{controls}\n--- name")).state,
                AgentState::Blocked
            );
        }
        for controls in [
            "controls: select-marker cancel-marker",
            "controls: navigate-marker cancel-marker",
            "controls: confirm-marker",
            "controls: select-marker unrelated navigate-marker cancel-marker",
            "controls: select-marker\nprompt: navigate-marker cancel-marker",
        ] {
            assert_eq!(
                explain(
                    Agent::Codex,
                    &format!("confirm-marker navigate-marker\n{controls}\n--- name")
                )
                .state,
                AgentState::Idle
            );
        }
    });
}

#[test]
fn paired_alternatives_require_complete_gates_at_bottom_buffer() {
    with_manifest_dirs("paired-alternatives", || {
        write_local_codex(&rules_manifest(
            r#"
[[rules]]
id = "paired"
state = "blocked"
region = "bottom_non_empty_lines(3)"
visible_blocker = true
any = [
  { all = [
    { contains = ["alpha heading"] },
    { regex = ['alpha\s+control\s*\z'] },
  ] },
  { all = [
    { contains = ["beta heading"] },
    { regex = ['beta\s+control\s*\z'] },
  ] },
]
"#,
        ));
        for text in [
            "alpha heading\nalpha control",
            "beta heading\nbeta\ncontrol",
        ] {
            let result = explain(Agent::Codex, text);
            assert_eq!(result.state, AgentState::Blocked);
            assert!(result.visible_blocker);
        }
        for text in [
            "alpha heading\nbeta control",
            "beta heading\nalpha control",
            "beta control",
            "beta heading",
            "beta heading\nbeta control\nnewer content",
            "beta heading\nfiller one\nfiller two\nbeta control",
        ] {
            let result = explain(Agent::Codex, text);
            assert!(result.matched_rule.is_none(), "{text}");
            assert!(!result.visible_blocker, "{text}");
        }
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
fn claude_rewind_checkpoint_picker_sets_select_without_changing_lifecycle() {
    let screen = "Restore the code and/or conversation to this checkpoint?\n\
        Enter to continue · Esc to cancel\n";
    let explain = explain(Agent::Claude, screen);

    assert_eq!(explain.state, AgentState::Idle);
    assert_eq!(
        explain.fallback_reason.as_deref(),
        Some(DEFAULT_KNOWN_AGENT_IDLE_FALLBACK)
    );
    assert!(explain.input_pending);
    assert_eq!(explain.input_prompt_kind, Some(InputPromptKind::Select));
    assert_eq!(
        explain
            .matched_input_rule
            .as_ref()
            .map(|rule| rule.id.as_str()),
        Some("rewind_checkpoint_picker")
    );
}

#[test]
fn claude_dynamic_workflow_dialog_sets_confirm_with_blocked_lifecycle() {
    let screen = "Run a dynamic workflow?\n\
        This workflow can make changes while it runs.\n\
        Esc to cancel\n";
    let explain = explain(Agent::Claude, screen);

    assert_eq!(explain.state, AgentState::Blocked);
    assert!(explain.visible_blocker);
    assert_eq!(
        explain.matched_rule.as_ref().map(|rule| rule.id.as_str()),
        Some("dynamic_workflow_prompt")
    );
    assert!(explain.input_pending);
    assert_eq!(explain.input_prompt_kind, Some(InputPromptKind::Confirm));
    assert_eq!(
        explain
            .matched_input_rule
            .as_ref()
            .map(|rule| rule.id.as_str()),
        Some("dynamic_workflow_confirm")
    );
}

#[test]
fn claude_enumerated_select_matches_captured_switch_model_dialog() {
    let screen = "Switch model?\nYour next response will be slower and use more tokens\n\nThis conversation is cached for the current model.\n\n❯ 1. Yes, switch to Sonnet 5\n  2. No, go back\n";
    let explain = explain(Agent::Claude, screen);

    assert!(explain.input_pending);
    assert_eq!(explain.input_prompt_kind, Some(InputPromptKind::Select));
    assert_eq!(
        explain
            .matched_input_rule
            .as_ref()
            .map(|rule| rule.id.as_str()),
        Some("claude_enumerated_select")
    );
    let selected = explain
        .evaluated_input_rules
        .iter()
        .find(|rule| rule.id == "claude_enumerated_select")
        .expect("enumerated rule");
    assert!(selected.matched);
}

#[test]
fn claude_enumerated_select_matches_later_rewind_restore_options() {
    let screen = "Rewind\nChoose what to restore from this checkpoint\n\n❯ 1. Restore code and conversation\n  2. Restore conversation only\n  3. Cancel\n";
    let explain = explain(Agent::Claude, screen);

    assert_eq!(explain.state, AgentState::Idle);
    assert!(explain.input_pending);
    assert_eq!(explain.input_prompt_kind, Some(InputPromptKind::Select));
    assert_eq!(
        explain
            .matched_input_rule
            .as_ref()
            .map(|rule| rule.id.as_str()),
        Some("claude_enumerated_select")
    );
}

#[test]
fn claude_enumerated_select_rejects_composer_numbered_lists() {
    let prompt_box = |body: &str| {
        format!(
            "previous output\n\
             ────────────────────────────────────────\n\
             {body}\n\
             ────────────────────────────────────────\n"
        )
    };
    for screen in [
        prompt_box("❯"),
        prompt_box("❯ 1. summarize this file"),
        prompt_box("❯ 1. fix the parser\n  2. add tests"),
        format!(
            "  2. the second approach\n{}",
            prompt_box("❯ 1. go with that")
        ),
    ] {
        let explain = explain(Agent::Claude, &screen);
        assert!(
            !explain.input_pending,
            "composer screen matched {:?}",
            explain.matched_input_rule
        );
    }
}

#[test]
fn claude_dialog_copy_in_transcript_is_not_live_input() {
    let screen = "Earlier transcript:\n❯ 1. Yes, switch model\n  2. No, go back\n────────────────────────────────────────\n❯ continue with the implementation\n────────────────────────────────────────\n";
    let explain = explain(Agent::Claude, screen);

    assert!(!explain.input_pending);
}

#[test]
fn rewind_copy_outside_live_bottom_region_is_not_input() {
    let screen = "Restore the code and/or conversation to this checkpoint?\n\
        Enter to continue · Esc to cancel\n\
        one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\n";
    let explain = explain(Agent::Claude, screen);

    assert!(!explain.input_pending);
}

#[test]
fn per_agent_input_rule_beats_shared_fallback_and_shared_unknown_is_typed() {
    let rewind = explain(
        Agent::Claude,
        "Restore the code and/or conversation to this checkpoint?\n\
         Enter to continue · Esc to cancel\n",
    );
    assert_eq!(
        rewind
            .matched_input_rule
            .as_ref()
            .map(|rule| (rule.id.as_str(), rule.shared, rule.kind)),
        Some(("rewind_checkpoint_picker", false, InputPromptKind::Select))
    );

    let generic = explain(
        Agent::Claude,
        "Provide a value\nPress Enter to continue\nEsc to cancel\n",
    );
    assert_eq!(
        generic
            .matched_input_rule
            .as_ref()
            .map(|rule| (rule.id.as_str(), rule.shared, rule.kind)),
        Some(("shared_enter_escape_footer", true, InputPromptKind::Unknown))
    );
}

#[test]
fn equal_priority_per_agent_input_rule_beats_shared_fallback() {
    let manifest = parse_manifest(
        r#"
id = "claude"

[[input_rules]]
id = "agent_enter_escape_confirm"
priority = 100
region = "bottom_non_empty_lines(5)"
kind = "confirm"
all = [
  { line_regex = ['(?i)\benter\b.+$'] },
  { line_regex = ['(?i)\besc(?:ape)?\s+to\s+cancel\b'] },
]
"#,
    )
    .expect("parse equal-priority agent rule");
    let loaded = loaded_manifest(manifest, ManifestSource::Bundled, None, None, false)
        .expect("load equal-priority agent rule");

    let input = DetectionInput {
        screen: "Confirm deployment\nEnter to continue\nEsc to cancel\n",
        osc_title: "",
        osc_progress: "",
    };
    let shared = shared_input_manifest();
    let mut winner = None;
    let mut evaluated_rules = Vec::new();
    evaluate_input_rule_set(
        &shared.manifest.input_rules,
        &shared.compiled_input_rules,
        input,
        true,
        &mut winner,
        &mut evaluated_rules,
    );
    assert_eq!(
        winner
            .as_ref()
            .map(|rule| (rule.id.as_str(), rule.priority, rule.shared)),
        Some(("shared_enter_escape_footer", 100, true)),
        "precondition: the shared rule wins first at the same priority"
    );
    evaluate_input_rule_set(
        &loaded.manifest.input_rules,
        &loaded.compiled_input_rules,
        input,
        false,
        &mut winner,
        &mut evaluated_rules,
    );

    assert_eq!(
        winner
            .as_ref()
            .map(|rule| (rule.id.as_str(), rule.shared, rule.kind)),
        Some((
            "agent_enter_escape_confirm",
            false,
            InputPromptKind::Confirm
        ))
    );
}

#[test]
fn ordinary_claude_composer_is_not_free_text_input() {
    let screen = "────────────────────────────────────────\n\
        ❯ explain the parser without changing it\n\
        ────────────────────────────────────────\n";
    let explain = explain(Agent::Claude, screen);

    assert!(!explain.input_pending);
    assert_ne!(explain.input_prompt_kind, Some(InputPromptKind::FreeText));
}

#[test]
fn claude_enumerated_rule_has_no_phrase_literals() {
    fn matcher_text(gate: &ManifestGate, values: &mut Vec<String>) {
        values.extend(gate.contains.iter().cloned());
        values.extend(gate.regex.iter().cloned());
        values.extend(gate.line_regex.iter().cloned());
        for nested in gate.all.iter().chain(&gate.any).chain(&gate.not_gate) {
            matcher_text(nested, values);
        }
    }

    let manifest = bundled_manifest(Agent::Claude).expect("Claude manifest");
    let rule = manifest
        .input_rules
        .iter()
        .find(|rule| rule.id == "claude_enumerated_select")
        .expect("enumerated select rule");
    let mut values = Vec::new();
    matcher_text(&manifest_gate_from_input_rule(rule), &mut values);
    let matcher_text = values.join("\n");
    for forbidden in ["switch model", "yes", "no", "restore", "enter", "esc"] {
        let forbidden = Regex::new(&format!(r"(?i)\b{}\b", regex::escape(forbidden))).unwrap();
        assert!(
            !forbidden.is_match(&matcher_text),
            "found forbidden phrase in matcher gates: {forbidden}"
        );
    }
}

#[test]
fn composer_visual_evidence_uses_cursor_without_text_equality() {
    let screen = "────────────────────────────────────────\n\
❯ [Pasted text #6]\n\
────────────────────────────────────────\n";
    let observation = composer_visual_observation(
        Agent::Claude,
        screen,
        Some(crate::pane::TerminalCursorState {
            x: 18,
            y: 1,
            visible: true,
            shape: 0,
        }),
        true,
        7,
    );
    assert_eq!(
        observation.region,
        crate::terminal::ComposerRegionObservation::Text
    );
    assert_eq!(
        observation.cursor,
        crate::terminal::ComposerCursorObservation::Draft
    );
}

#[test]
fn composer_visual_evidence_distinguishes_suggestion_right_of_cursor() {
    let screen = "────────────────────────────────────────\n\
❯  suggestion\n\
────────────────────────────────────────\n";
    let observation = composer_visual_observation(
        Agent::Claude,
        screen,
        Some(crate::pane::TerminalCursorState {
            x: 2,
            y: 1,
            visible: true,
            shape: 0,
        }),
        true,
        7,
    );
    assert_eq!(
        observation.cursor,
        crate::terminal::ComposerCursorObservation::Suggestion
    );
}

#[test]
fn missing_composer_region_is_explicit() {
    let observation = composer_visual_observation(Agent::Claude, "no prompt box", None, true, 7);
    assert_eq!(
        observation.region,
        crate::terminal::ComposerRegionObservation::Missing
    );
}

#[test]
fn devin_manifest_detects_idle_working_and_blocked_states() {
    let idle = explain(
        Agent::Devin,
        "─────────────────────────────────────────────────────\n❭ Ask Devin to build features, fix bugs, or work on\n  your code\n─────────────────────────────────────────────────────\nSWE-1.6               Context: 16k / 200k tokens (7%)",
    );
    assert_eq!(idle.state, AgentState::Idle);
    assert!(idle.visible_idle);

    let live_footer_idle = explain(
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

    let welcome_footer_idle = explain(
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

    let working = explain(
        Agent::Devin,
        "◔ Reading shell 91b655\n  │ Timeout: 35s\n\n⠀⡆ Running tools · 27s (esc to interrupt)\n─────────────────────────────────────────────────────\n❭ Guide Devin while it works",
    );
    assert_eq!(working.state, AgentState::Working);
    assert!(working.visible_working);

    let trust_prompt = explain(
        Agent::Devin,
        "Do you trust the authors of this directory?\nFor security, devin should not be run in directories\nwith untrusted content.\n❭ 1 Yes, trust /private/tmp/devin-hook-probe\n· 2 No, exit",
    );
    assert_eq!(trust_prompt.state, AgentState::Blocked);
    assert!(trust_prompt.visible_blocker);

    let permission_prompt = explain(
        Agent::Devin,
        "⏺ Running command\n  └ $ sleep 30\n\n❭ 1 Yes  (Approve once)\n· 2 Yes, allow `sleep` commands\n· 3 Yes, always allow `sleep` commands\n· 4 No\n↑↓ select · ↵ confirm · esc cancel",
    );
    assert_eq!(permission_prompt.state, AgentState::Blocked);
    assert!(permission_prompt.visible_blocker);
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
