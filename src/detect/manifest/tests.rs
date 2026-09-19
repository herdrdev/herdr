use super::*;

fn versioned_manifest(version: &str, state: &str, contains: &str) -> String {
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

fn historical_remote_path() -> PathBuf {
    crate::config::state_dir().join("agent-detection/remote/codex.toml")
}

fn write_historical_remote(content: &str) {
    let path = historical_remote_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn write_local_codex_without_reload(content: &str) {
    let path = override_path(Agent::Codex).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn selected_codex_registry() -> crate::agents::AgentRegistry {
    let detection = versioned_manifest("7.2.1", "blocked", "package-ready");
    let packages = crate::agents::source::load_packages(&[
        ("agents/codex/agent.toml", "schema = 1\nid = 'codex'\nname = 'Codex'\naliases = []\nstartable = true\n[launch]\nunix = 'codex'\nwindows = 'codex'\n"),
        ("agents/codex/detection.toml", detection.as_str()),
    ]).unwrap();
    crate::agents::AgentRegistry::from_packages(packages).unwrap()
}

fn cache_explain(cache: &ManifestCache, screen: &str) -> DetectionExplain {
    explain_with_cache(
        cache,
        Agent::Codex,
        DetectionInput {
            screen,
            osc_title: "",
            osc_progress: "",
        },
        true,
    )
}

fn test_remote_revision() -> crate::agents::remote::RemoteRevision {
    crate::agents::remote::RemoteRevision {
        origin: "https://registry.herdr.dev".into(),
        pointer: crate::agents::remote::ChannelPointer {
            schema: 1,
            channel: crate::agents::remote::Channel::Stable,
            generation: 9,
            snapshot_sha256: "a".repeat(64),
            snapshot_bytes: 100,
        },
        commit: "b".repeat(40),
    }
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
fn historical_website_cache_and_status_are_ignored_and_preserved() {
    with_manifest_dirs("historical-cache", || {
        let historical = versioned_manifest("9999.1", "working", "historical-ready");
        write_historical_remote(&historical);
        let status_path = crate::config::state_dir().join("agent-detection/status.toml");
        let historical_status = "last_check_unix = 123\nlast_result = 'historical-website'\n";
        std::fs::write(&status_path, historical_status).unwrap();
        reload_manifests();
        let explanation = explain(Agent::Codex, "historical-ready");
        assert_eq!(explanation.source, Some(ManifestSource::Bundled));
        assert_eq!(explanation.cached_remote_version, None);
        assert_eq!(explanation.remote_update_status, None);
        assert_eq!(explanation.warning, None);
        assert_eq!(
            std::fs::read_to_string(historical_remote_path()).unwrap(),
            historical
        );
        assert_eq!(
            std::fs::read_to_string(&status_path).unwrap(),
            historical_status
        );
        let status = crate::detect::manifest_compat::load_status();
        assert_eq!(status.last_check_unix, None);
        assert_eq!(
            status.last_result.as_deref(),
            Some("active registry snapshot")
        );
        assert!(status.agents.is_empty());
    });
}

#[test]
fn selected_package_provenance_and_versions_are_not_website_cache_or_snapshot_hashes() {
    with_manifest_dirs("package-provenance", || {
        write_historical_remote(&versioned_manifest("9999.1", "working", "package-ready"));
        let registry = selected_codex_registry();
        let mut cache = build_manifest_cache(&registry);
        apply_registry_provenance(&mut cache, Some(Path::new("/selected/agents")), None);
        let local = cache_explain(&cache, "package-ready");
        assert_eq!(local.state, AgentState::Blocked);
        assert_eq!(
            local.source,
            Some(ManifestSource::LocalRegistry("/selected/agents".into()))
        );
        assert_eq!(local.cached_remote_version, None);
        assert_eq!(local.manifest_version.as_deref(), Some("7.2.1"));

        let remote = test_remote_revision();
        apply_registry_provenance(&mut cache, None, Some(&remote));
        let accepted = cache_explain(&cache, "package-ready");
        assert!(matches!(
            accepted.source,
            Some(ManifestSource::R2 { generation: 9, .. })
        ));
        assert_eq!(accepted.cached_remote_version.as_deref(), Some("7.2.1"));
        assert_eq!(
            accepted.remote_update_status.as_deref(),
            Some("accepted_registry")
        );
        let fallback = cache_explain(&cache, "no match");
        assert_eq!(fallback.manifest_version.as_deref(), Some("7.2.1"));
        assert_eq!(fallback.cached_remote_version.as_deref(), Some("7.2.1"));
        assert_eq!(fallback.remote_update_status, accepted.remote_update_status);
        apply_registry_provenance(&mut cache, None, None);
        assert_eq!(
            cache_explain(&cache, "package-ready").source,
            Some(ManifestSource::Bundled)
        );
        assert_eq!(
            cache_explain(&cache, "package-ready").cached_remote_version,
            None
        );
    });
}

#[test]
fn local_override_is_preserved_over_selected_local_and_r2_packages() {
    with_manifest_dirs("override-selected-package", || {
        let content = versioned_manifest("8.1", "working", "local-ready");
        write_local_codex_without_reload(&content);
        let registry = selected_codex_registry();
        let mut cache = build_manifest_cache(&registry);
        apply_registry_provenance(&mut cache, Some(Path::new("/selected/agents")), None);
        let local = cache_explain(&cache, "local-ready");
        assert_eq!(local.state, AgentState::Working);
        assert!(matches!(local.source, Some(ManifestSource::Override(_))));
        assert!(!local.local_override_shadowing_remote);
        apply_registry_provenance(&mut cache, None, Some(&test_remote_revision()));
        let remote = cache_explain(&cache, "local-ready");
        assert_eq!(remote.manifest_version.as_deref(), Some("8.1"));
        assert_eq!(remote.cached_remote_version.as_deref(), Some("7.2.1"));
        assert!(remote.local_override_shadowing_remote);
        assert_eq!(
            std::fs::read_to_string(override_path(Agent::Codex).unwrap()).unwrap(),
            content
        );
        // A source replacement lacking this profile cannot resurrect its override.
        let empty = build_manifest_cache(&crate::agents::AgentRegistry::default());
        assert!(summaries(&empty).is_empty());
        assert_eq!(cache_explain(&empty, "local-ready").source, None);
    });
}

#[test]
fn invalid_local_override_falls_back_to_selected_package_with_warning() {
    with_manifest_dirs("invalid-override-selected", || {
        let registry = selected_codex_registry();
        for content in [
            "id = ".to_string(),
            local_manifest("working", "package-ready").replace("codex", "cursor"),
        ] {
            write_local_codex_without_reload(&content);
            let mut cache = build_manifest_cache(&registry);
            apply_registry_provenance(&mut cache, None, Some(&test_remote_revision()));
            let explanation = cache_explain(&cache, "package-ready");
            assert_eq!(explanation.state, AgentState::Blocked);
            assert!(matches!(
                explanation.source,
                Some(ManifestSource::R2 { .. })
            ));
            assert!(explanation
                .warning
                .as_deref()
                .unwrap()
                .contains("ignored override"));
            assert!(!explanation.local_override_shadowing_remote);
            assert_eq!(explanation.cached_remote_version.as_deref(), Some("7.2.1"));
            assert_eq!(
                std::fs::read_to_string(override_path(Agent::Codex).unwrap()).unwrap(),
                content
            );
        }
    });
}

#[test]
fn detection_uses_cached_local_override_until_explicit_reload() {
    with_manifest_dirs("cache-boundary", || {
        write_local_codex(&versioned_manifest(
            "9999.01.01.1",
            "blocked",
            "cached-ready",
        ));
        assert_eq!(
            explain(Agent::Codex, "cached-ready").state,
            AgentState::Blocked
        );
        write_local_codex_without_reload(&versioned_manifest(
            "9999.01.01.2",
            "working",
            "new-ready",
        ));
        let unchanged = explain(Agent::Codex, "new-ready");
        assert_eq!(unchanged.state, AgentState::Idle);
        assert_eq!(unchanged.manifest_version.as_deref(), Some("9999.01.01.1"));
        reload_manifests();
        let reloaded = explain(Agent::Codex, "new-ready");
        assert_eq!(reloaded.state, AgentState::Working);
        assert_eq!(reloaded.manifest_version.as_deref(), Some("9999.01.01.2"));
    });
}

#[test]
fn compiled_rules_are_shared_until_manifest_reload() {
    with_manifest_dirs("shared-compiled-rules", || {
        write_local_codex(&format!(
            "{}\nregex = ['^cached-[a-z]+$']\n",
            versioned_manifest("9999.01.01.1", "blocked", "cached-ready")
        ));
        let snapshot = crate::agents::registry();
        let first = load_manifest(&snapshot.manifests, Agent::Codex).unwrap();
        let second = load_manifest(&snapshot.manifests, Agent::Codex).unwrap();
        assert!(!first.compiled_rules.is_empty());
        assert_eq!(
            first.compiled_rules.as_ptr(),
            second.compiled_rules.as_ptr(),
            "cached loads must retain the same compiled rules and regex search caches"
        );

        write_local_codex_without_reload(&format!(
            "{}\nregex = ['^new-[a-z]+$']\n",
            versioned_manifest("9999.01.01.2", "working", "new-ready")
        ));
        let unchanged = load_manifest(&snapshot.manifests, Agent::Codex).unwrap();
        assert_eq!(
            first.compiled_rules.as_ptr(),
            unchanged.compiled_rules.as_ptr()
        );

        reload_manifests_for_agents(&[Agent::Codex]);
        let snapshot = crate::agents::registry();
        let reloaded = load_manifest(&snapshot.manifests, Agent::Codex).unwrap();
        let shared_reload = load_manifest(&snapshot.manifests, Agent::Codex).unwrap();
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
                    let snapshot = crate::agents::registry();
                    let loaded = load_manifest(&snapshot.manifests, Agent::Codex).unwrap();
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
        assert!(!detect(Agent::Codex, "activity-marker").skip_state_update);
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
fn source_tree_agent_directories_and_manifests_match_registry() {
    let agents_root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("vendor/agent-registry/agents");
    let mut source_directories = Vec::new();
    for entry in std::fs::read_dir(&agents_root).expect("agent source directory should be readable")
    {
        let entry = entry.expect("agent source entry should be readable");
        if entry
            .file_type()
            .expect("agent source entry type should be readable")
            .is_dir()
        {
            source_directories.push(
                entry
                    .file_name()
                    .into_string()
                    .expect("agent source directory names should be UTF-8"),
            );
        }
    }
    source_directories.sort();

    let registry = crate::agents::registry();
    let mut registered_directories: Vec<_> = registry
        .known_profiles()
        .map(|profile| profile.canonical_id().to_string())
        .collect();
    registered_directories.sort();
    assert_eq!(source_directories, registered_directories);

    for profile in registry.known_profiles() {
        let manifest_path = agents_root
            .join(profile.canonical_id())
            .join("detection.toml");
        let detection = profile.detection();
        assert_eq!(
            manifest_path.is_file(),
            detection.is_some(),
            "detection file capability mismatch for {}",
            profile.canonical_id()
        );

        let Some(detection) = detection else {
            continue;
        };
        let content = std::fs::read_to_string(&manifest_path)
            .expect("registered detection manifest should be readable");
        assert_eq!(content, detection);

        let parsed = parse_manifest(&content).expect("bundled manifest should parse");
        assert_eq!(parsed.id, profile.canonical_id());
        assert!(bundled_manifest(&crate::agents::registry(), profile.legacy_agent()).is_some());
    }
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
#[test]
fn retained_snapshot_keeps_compiled_detection_after_reload() {
    with_manifest_dirs("retained-snapshot", || {
        write_local_codex(&versioned_manifest(
            "9999.01.01.1",
            "blocked",
            "snapshot-ready",
        ));
        let retained = crate::agents::registry();
        write_local_codex_without_reload(&versioned_manifest(
            "9999.01.01.2",
            "working",
            "snapshot-ready",
        ));
        reload_manifests();
        let current = crate::agents::registry();
        let input = DetectionInput {
            screen: "snapshot-ready",
            osc_title: "",
            osc_progress: "",
        };

        assert_eq!(
            detect_with_registry(&retained, Agent::Codex, input).state,
            AgentState::Blocked
        );
        assert_eq!(
            detect_with_registry(&current, Agent::Codex, input).state,
            AgentState::Working
        );
        assert_eq!(
            explain_with_registry(&retained, Agent::Codex, input)
                .manifest_version
                .as_deref(),
            Some("9999.01.01.1")
        );
        assert!(std::sync::Arc::ptr_eq(
            &retained.registry,
            &current.registry
        ));
        assert_eq!(retained.digest, current.digest);
    });
}

#[test]
fn selective_reload_retains_unselected_local_override_exactly() {
    with_manifest_dirs("selective-local-snapshot", || {
        let path = override_path(Agent::Cursor).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            local_manifest("blocked", "retained-local").replace("codex", "cursor"),
        )
        .unwrap();
        reload_manifests();
        let before = crate::agents::registry();
        let cursor_summary = summaries(&before.manifests)
            .into_iter()
            .find(|summary| summary.agent == Agent::Cursor)
            .unwrap();

        // Both files change, but only Codex is part of this publication.
        std::fs::write(
            &path,
            local_manifest("working", "changed-local").replace("codex", "cursor"),
        )
        .unwrap();
        write_local_codex_without_reload(&versioned_manifest(
            "9999.01.01.2",
            "working",
            "fresh-remote",
        ));
        reload_manifests_for_agents(&[Agent::Codex]);
        let after = crate::agents::registry();
        assert_eq!(
            summaries(&after.manifests)
                .into_iter()
                .find(|summary| summary.agent == Agent::Cursor)
                .unwrap(),
            cursor_summary
        );
        assert_eq!(
            explain(Agent::Cursor, "retained-local").state,
            AgentState::Blocked
        );
        assert_eq!(
            explain(Agent::Cursor, "changed-local").state,
            AgentState::Idle
        );
        assert_eq!(
            explain(Agent::Codex, "fresh-remote").state,
            AgentState::Working
        );
        assert!(std::sync::Arc::ptr_eq(&before.registry, &after.registry));
    });
}

#[test]
fn candidate_alias_matching_does_not_use_published_registry() {
    let manifest =
        parse_manifest(&local_manifest("working", "ready").replace("codex", "claude-code"))
            .unwrap();
    let published = crate::agents::registry();
    assert!(manifest_matches_agent(&published, &manifest, Agent::Claude));
    assert!(!manifest_matches_agent(
        &crate::agents::AgentRegistry::default(),
        &manifest,
        Agent::Claude
    ));
}

#[test]
fn empty_candidate_detection_keeps_known_agent_idle_fallback() {
    let registry = crate::agents::AgentRegistry::default();
    let manifests = build_manifest_cache(&registry);
    assert!(summaries(&manifests).is_empty());
    let explanation = explain_with_cache(
        &manifests,
        Agent::Codex,
        DetectionInput {
            screen: "anything",
            osc_title: "",
            osc_progress: "",
        },
        false,
    );
    assert_eq!(explanation.state, AgentState::Idle);
    assert_eq!(explanation.source, None);
    assert_eq!(
        explanation.fallback_reason.as_deref(),
        Some(DEFAULT_KNOWN_AGENT_IDLE_FALLBACK)
    );
}

#[test]
fn strict_readiness_requires_a_screen_bound_visible_idle_rule() {
    fn snapshot(
        region: &str,
        visible_idle: bool,
    ) -> std::sync::Arc<crate::agents::RegistrySnapshot> {
        crate::agents::store::snapshot_for_test(
            vec![
                (
                    "agents/strict-test/agent.toml".into(),
                    "schema = 1\nid = 'strict-test'\nname = 'strict-test'\naliases = []\nstartable = true\n[launch]\nunix = 'strict-test'\nwindows = 'strict-test'\n".into(),
                ),
                (
                    "agents/strict-test/process.toml".into(),
                    "names = ['strict-test']\n".into(),
                ),
                (
                    "agents/strict-test/detection.toml".into(),
                    format!(
                        "id = 'strict-test'\nversion = '2026.06.10.1'\nmin_engine_version = 1\n[[rules]]\nid = 'idle'\nstate = 'idle'\npriority = 10\nregion = '{region}'\nvisible_idle = {visible_idle}\ncontains = ['ready']\n"
                    ),
                ),
            ],
            1,
        )
        .unwrap()
    }

    let agent = crate::detect::Agent::parse("strict-test").unwrap();
    assert!(super::requires_screen_visible_idle(
        &snapshot("bottom_non_empty_lines(4)", true),
        agent
    ));
    assert!(!super::requires_screen_visible_idle(
        &snapshot("osc_title", true),
        agent
    ));
    assert!(!super::requires_screen_visible_idle(
        &snapshot("bottom_lines(4)", false),
        agent
    ));
    for (region, screen_visible_idle) in [
        ("bottom_non_empty_lines(4)", true),
        ("osc_title", false),
        ("osc_progress", false),
    ] {
        let result = super::detect_with_registry(
            &snapshot(region, true),
            agent,
            DetectionInput {
                screen: "ready",
                osc_title: "ready",
                osc_progress: "ready",
            },
        );
        assert_eq!(result.state, AgentState::Idle);
        assert!(result.visible_idle, "OSC idle retains its status evidence");
        assert_eq!(result.screen_visible_idle, screen_visible_idle, "{region}");
    }
}

#[test]
fn opencode_visible_idle_uses_structural_bottom_controls_at_wide_and_narrow_widths() {
    for screen in [
        "  ┃\n  ┃  Ask anything...\n  ┃\n  ┃  Build · model\n  ╹▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀\n  tab agents  key commands\n",
        "  ┃\n  ┃  Build · model name\n  ┃          wrapped\n  ╹▀▀▀▀▀▀▀▀▀▀▀▀\n  tab agents\n  other-key commands\n",
        "  prior conversation\n  ┃\n  ┃  Build · model\n  ╹▀▀▀▀▀▀▀▀▀▀▀▀\n  project 2 (0%) other-key\n  commands\n",
    ] {
        let result = super::detect(Agent::OpenCode, screen);
        assert_eq!(result.state, AgentState::Idle);
        assert!(result.visible_idle);
        assert!(!result.skip_state_update);
    }
}

#[test]
fn opencode_priority_keeps_palette_working_and_blocked_surfaces_out_of_idle() {
    let palette_with_controls_behind =
        "╹▀▀▀▀▀▀▀▀▀▀▀▀\ncommands\nCommands   esc\nSearch\nSuggested\nSwitch model\n";
    let palette = super::detect(Agent::OpenCode, palette_with_controls_behind);
    assert_eq!(palette.state, AgentState::Unknown);
    assert!(palette.skip_state_update);
    assert!(!palette.visible_idle);

    let working = super::detect(Agent::OpenCode, "┃\n╹▀▀▀▀▀▀▀▀▀▀▀▀\ncommands\n■■■■■■\n");
    assert_eq!(working.state, AgentState::Working);
    assert!(working.visible_working);
    assert!(!working.visible_idle);

    let blocked = super::detect(
        Agent::OpenCode,
        "┃\n╹▀▀▀▀▀▀▀▀▀▀▀▀\ncommands\n△ Permission required\n",
    );
    assert_eq!(blocked.state, AgentState::Blocked);
    assert!(blocked.visible_blocker);
    assert!(!blocked.visible_idle);

    let blank = super::detect(Agent::OpenCode, "");
    assert_eq!(blank.state, AgentState::Unknown);
    assert!(blank.skip_state_update);
    assert!(!blank.visible_idle);
}
