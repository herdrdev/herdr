use super::*;

#[test]
fn bundled_packages_pass_the_same_full_validation_as_local_source() {
    let packages = validate_packages(bundled::FILES).expect("valid bundled registry");
    assert_eq!(packages.len(), Agent::ALL.len());
    let loaded = AgentRegistry::from_packages(packages).expect("valid identities");
    assert_eq!(loaded.known_profiles().len(), Agent::ALL.len());
}

#[test]
fn package_removal_preserves_core_report_policy_without_activating_capabilities() {
    let absent = AgentRegistry::default();
    let bundled = registry();
    for profile in bundled.known_profiles() {
        let id = profile.canonical_id();
        let Some(source) = profile.report().official_source() else {
            continue;
        };
        assert!(absent.profile_by_id(id).is_none());
        assert!(absent
            .session_profile_for_exact_report_pair(source, id)
            .is_none());
        assert_eq!(
            absent.report_policy_for_exact_pair(source, id),
            bundled.report_policy_for_exact_pair(source, id),
        );
        assert!(absent
            .report_policy_for_exact_pair("herdr:launch", id)
            .is_none());
        assert!(absent
            .report_policy_for_exact_pair(source, "novel-agent")
            .is_none());
    }
    assert!(absent.is_reserved_native_state_source("herdr:codex", "codex"));
    assert!(absent.has_full_lifecycle_report_authority("herdr:opencode", "opencode"));
    assert!(!absent.has_full_lifecycle_report_authority("herdr:opencode", "open-code"));
}

#[test]
fn novel_source_identity_can_launch_without_gaining_report_or_installer_authority() {
    let packages = validate_packages(&[(
        "agents/example/agent.toml",
        r#"
schema = 1
id = "example"
name = "Example"
aliases = []
startable = true
[launch]
unix = "example"
windows = "example"
"#,
    )])
    .expect("new package can be parsed without a Rust enum variant");
    assert_eq!(packages[0].identity.id, "example");
    let registry = AgentRegistry::from_packages(packages).expect("valid novel identity");
    let id = Agent::parse("example").expect("canonical identity");
    assert_eq!(
        registry
            .profile_by_id("example")
            .map(|profile| profile.legacy_agent()),
        Some(id)
    );
    assert!(registry
        .profile_by_normalized_process_name("example")
        .is_none());
    assert_eq!(
        registry
            .known_profiles()
            .filter(|profile| profile.is_startable())
            .map(|profile| profile.legacy_agent())
            .collect::<Vec<_>>(),
        vec![id]
    );
    assert!(registry.integration_capable_profiles().next().is_none());
    assert!(!registry.has_full_lifecycle_report_authority("herdr:example", "example"));
    assert!(registry.profile_by_agent(Agent::Pi).is_none());
}

#[test]
fn owned_registry_preserves_identity_indexes_independent_of_input_order() {
    let mut packages = validate_packages(bundled::FILES).expect("valid bundled registry");
    packages.reverse();
    let registry = AgentRegistry::from_packages(packages).expect("valid identities");
    for agent in Agent::ALL {
        let profile = registry.profile_by_agent(agent).expect("bound identity");
        assert_eq!(profile.canonical_id(), agent.as_str());
        assert!(std::ptr::eq(
            profile,
            registry.profile_by_id(agent.as_str()).expect("id lookup")
        ));
    }
}

const EXPECTED_IDENTITIES: [(Agent, &str, &[&str], &str, &str); 23] = [
    (Agent::Pi, "pi", &[], "pi", "pi"),
    (
        Agent::Claude,
        "claude",
        &["claude-code"],
        "claude",
        "claude",
    ),
    (Agent::Codex, "codex", &[], "codex", "codex"),
    (Agent::Gemini, "gemini", &[], "gemini", "gemini"),
    (
        Agent::Cursor,
        "cursor",
        &["cursor-agent"],
        "cursor-agent",
        "cursor-agent.cmd",
    ),
    (
        Agent::Devin,
        "devin",
        &["devin-cli", "devin cli"],
        "devin",
        "devin",
    ),
    (
        Agent::Antigravity,
        "agy",
        &["antigravity", "antigravity-cli"],
        "agy",
        "agy",
    ),
    (Agent::Cline, "cline", &[], "cline", "cline"),
    (Agent::Omp, "omp", &[], "omp", "omp"),
    (
        Agent::Mastracode,
        "mastracode",
        &["mastra-code", "mastra code"],
        "mastracode",
        "mastracode",
    ),
    (
        Agent::OpenCode,
        "opencode",
        &["opencode2", "open-code"],
        "opencode",
        "opencode",
    ),
    (
        Agent::GithubCopilot,
        "copilot",
        &["github-copilot", "ghcs"],
        "copilot",
        "copilot",
    ),
    (
        Agent::Kimi,
        "kimi",
        &["kimi-code", "kimi code"],
        "kimi",
        "kimi",
    ),
    (Agent::Kiro, "kiro", &["kiro-cli"], "kiro-cli", "kiro-cli"),
    (Agent::Droid, "droid", &[], "droid", "droid"),
    (Agent::Amp, "amp", &["amp-local"], "amp", "amp"),
    (Agent::Grok, "grok", &["grok-build"], "grok", "grok"),
    (
        Agent::Hermes,
        "hermes",
        &["hermes-agent"],
        "hermes",
        "hermes",
    ),
    (
        Agent::Kilo,
        "kilo",
        &["kilo-code", "kilo code"],
        "kilo",
        "kilo",
    ),
    (
        Agent::Qodercli,
        "qodercli",
        &["qoderclicn", "qoder", "qodercn"],
        "qodercli",
        "qodercli",
    ),
    (
        Agent::Qwen,
        "qwen",
        &["qwen-code", "qwen code"],
        "qwen",
        "qwen",
    ),
    (Agent::Maki, "maki", &[], "maki", "maki"),
    (
        Agent::Muse,
        "muse",
        &["muse-code", "muse-cli"],
        "muse",
        "muse",
    ),
];

type ExpectedReportPolicy = (
    Agent,
    Option<&'static str>,
    ReportAuthority,
    bool,
    &'static [&'static str],
    bool,
    Option<&'static str>,
    bool,
);

const EXPECTED_REPORT_POLICIES: [ExpectedReportPolicy; 23] = [
    (
        Agent::Pi,
        Some("herdr:pi"),
        ReportAuthority::FullLifecycle,
        false,
        &["new", "resume", "fork"],
        false,
        None,
        false,
    ),
    (
        Agent::Claude,
        Some("herdr:claude"),
        ReportAuthority::None,
        true,
        &["clear", "resume", "compact"],
        false,
        None,
        false,
    ),
    (
        Agent::Codex,
        Some("herdr:codex"),
        ReportAuthority::None,
        true,
        &["startup", "clear", "resume", "compact"],
        false,
        None,
        false,
    ),
    (
        Agent::Gemini,
        None,
        ReportAuthority::None,
        false,
        &[],
        false,
        None,
        false,
    ),
    (
        Agent::Cursor,
        Some("herdr:cursor"),
        ReportAuthority::None,
        true,
        &[],
        false,
        None,
        false,
    ),
    (
        Agent::Devin,
        Some("herdr:devin"),
        ReportAuthority::None,
        true,
        &[],
        false,
        None,
        false,
    ),
    (
        Agent::Antigravity,
        Some("herdr:antigravity_cli"),
        ReportAuthority::SessionIdentityOnly,
        false,
        &[],
        true,
        None,
        false,
    ),
    (
        Agent::Cline,
        None,
        ReportAuthority::None,
        false,
        &[],
        false,
        None,
        false,
    ),
    (
        Agent::Omp,
        Some("herdr:omp"),
        ReportAuthority::FullLifecycle,
        false,
        &["startup", "new", "resume", "fork"],
        false,
        None,
        false,
    ),
    (
        Agent::Mastracode,
        Some("herdr:mastracode"),
        ReportAuthority::FullLifecycle,
        false,
        &["startup"],
        false,
        None,
        true,
    ),
    (
        Agent::OpenCode,
        Some("herdr:opencode"),
        ReportAuthority::FullLifecycle,
        false,
        &["select"],
        false,
        Some("select"),
        false,
    ),
    (
        Agent::GithubCopilot,
        Some("herdr:copilot"),
        ReportAuthority::None,
        true,
        &[],
        false,
        None,
        false,
    ),
    (
        Agent::Kimi,
        Some("herdr:kimi"),
        ReportAuthority::FullLifecycle,
        false,
        &[],
        false,
        None,
        false,
    ),
    (
        Agent::Kiro,
        None,
        ReportAuthority::None,
        false,
        &[],
        false,
        None,
        false,
    ),
    (
        Agent::Droid,
        Some("herdr:droid"),
        ReportAuthority::None,
        true,
        &[],
        false,
        None,
        false,
    ),
    (
        Agent::Amp,
        None,
        ReportAuthority::None,
        false,
        &[],
        false,
        None,
        false,
    ),
    (
        Agent::Grok,
        Some("herdr:grok"),
        ReportAuthority::None,
        true,
        &[],
        false,
        None,
        false,
    ),
    (
        Agent::Hermes,
        Some("herdr:hermes"),
        ReportAuthority::SessionIdentityOnly,
        false,
        &["startup", "new", "resume"],
        false,
        None,
        false,
    ),
    (
        Agent::Kilo,
        Some("herdr:kilo"),
        ReportAuthority::FullLifecycle,
        false,
        &[],
        false,
        None,
        false,
    ),
    (
        Agent::Qodercli,
        Some("herdr:qodercli"),
        ReportAuthority::None,
        true,
        &[],
        false,
        None,
        false,
    ),
    (
        Agent::Qwen,
        Some("herdr:qwen"),
        ReportAuthority::SessionIdentityOnly,
        true,
        &["startup", "clear", "resume", "compact", "branch"],
        false,
        None,
        false,
    ),
    (
        Agent::Maki,
        None,
        ReportAuthority::None,
        false,
        &[],
        false,
        None,
        false,
    ),
    (
        Agent::Muse,
        None,
        ReportAuthority::None,
        false,
        &[],
        false,
        None,
        false,
    ),
];

const EXPECTED_SCREEN_DETECTABLE: [Agent; 21] = [
    Agent::Pi,
    Agent::Claude,
    Agent::Codex,
    Agent::Gemini,
    Agent::Cursor,
    Agent::Devin,
    Agent::Antigravity,
    Agent::Cline,
    Agent::OpenCode,
    Agent::GithubCopilot,
    Agent::Kimi,
    Agent::Kiro,
    Agent::Droid,
    Agent::Amp,
    Agent::Grok,
    Agent::Hermes,
    Agent::Kilo,
    Agent::Qodercli,
    Agent::Qwen,
    Agent::Maki,
    Agent::Muse,
];

const EXPECTED_RESUMABLE: [Agent; 17] = [
    Agent::Pi,
    Agent::Claude,
    Agent::Codex,
    Agent::Cursor,
    Agent::Devin,
    Agent::Antigravity,
    Agent::Omp,
    Agent::Mastracode,
    Agent::OpenCode,
    Agent::GithubCopilot,
    Agent::Kimi,
    Agent::Droid,
    Agent::Grok,
    Agent::Hermes,
    Agent::Kilo,
    Agent::Qodercli,
    Agent::Qwen,
];

const EXPECTED_INTEGRATION_CAPABLE: [Agent; 17] = [
    Agent::Pi,
    Agent::Omp,
    Agent::Claude,
    Agent::Codex,
    Agent::GithubCopilot,
    Agent::Devin,
    Agent::Droid,
    Agent::Kimi,
    Agent::OpenCode,
    Agent::Kilo,
    Agent::Hermes,
    Agent::Qodercli,
    Agent::Qwen,
    Agent::Cursor,
    Agent::Mastracode,
    Agent::Antigravity,
    Agent::Grok,
];

type ExpectedIntegrationProfile = (
    IntegrationTarget,
    Agent,
    &'static str,
    &'static [&'static str],
    &'static [&'static str],
    &'static [&'static str],
    u32,
    u32,
);

const EXPECTED_INTEGRATION_PROFILES: [ExpectedIntegrationProfile; 17] = [
    (
        IntegrationTarget::Pi,
        Agent::Pi,
        "pi",
        &[],
        &["pi"],
        &["pi"],
        9,
        9,
    ),
    (
        IntegrationTarget::Omp,
        Agent::Omp,
        "omp",
        &[],
        &["omp"],
        &["omp"],
        9,
        9,
    ),
    (
        IntegrationTarget::Claude,
        Agent::Claude,
        "claude",
        &[],
        &["claude"],
        &["claude"],
        9,
        9,
    ),
    (
        IntegrationTarget::Codex,
        Agent::Codex,
        "codex",
        &[],
        &["codex"],
        &["codex"],
        8,
        8,
    ),
    (
        IntegrationTarget::Copilot,
        Agent::GithubCopilot,
        "copilot",
        &[],
        &["copilot"],
        &["copilot"],
        3,
        3,
    ),
    (
        IntegrationTarget::Devin,
        Agent::Devin,
        "devin",
        &[],
        &["devin"],
        &["devin"],
        2,
        2,
    ),
    (
        IntegrationTarget::Droid,
        Agent::Droid,
        "droid",
        &[],
        &["droid"],
        &["droid"],
        3,
        3,
    ),
    (
        IntegrationTarget::Kimi,
        Agent::Kimi,
        "kimi",
        &[],
        &["kimi"],
        &["kimi"],
        7,
        7,
    ),
    (
        IntegrationTarget::Opencode,
        Agent::OpenCode,
        "opencode",
        &[],
        &["opencode"],
        &["opencode"],
        11,
        11,
    ),
    (
        IntegrationTarget::Kilo,
        Agent::Kilo,
        "kilo",
        &[],
        &["kilo", "kilo-code"],
        &["kilo", "kilo-code"],
        4,
        4,
    ),
    (
        IntegrationTarget::Hermes,
        Agent::Hermes,
        "hermes",
        &[],
        &["hermes"],
        &["hermes"],
        5,
        5,
    ),
    (
        IntegrationTarget::Qodercli,
        Agent::Qodercli,
        "qodercli",
        &[],
        &["qodercli"],
        &["qodercli", "qoder", "qoderclicn", "qodercn"],
        3,
        3,
    ),
    (
        IntegrationTarget::Qwen,
        Agent::Qwen,
        "qwen",
        &[],
        &["qwen"],
        &["qwen"],
        1,
        1,
    ),
    (
        IntegrationTarget::Cursor,
        Agent::Cursor,
        "cursor",
        &[],
        &["cursor-agent"],
        &["cursor-agent"],
        1,
        1,
    ),
    (
        IntegrationTarget::Mastracode,
        Agent::Mastracode,
        "mastracode",
        &[],
        &["mastracode"],
        &["mastracode"],
        2,
        2,
    ),
    (
        IntegrationTarget::AntigravityCli,
        Agent::Antigravity,
        "antigravity-cli",
        &["antigravity_cli"],
        &["agy"],
        &["agy"],
        3,
        3,
    ),
    (
        IntegrationTarget::Grok,
        Agent::Grok,
        "grok",
        &[],
        &["grok"],
        &["grok"],
        1,
        1,
    ),
];

fn profile_agents<'a>(profiles: impl Iterator<Item = &'a AgentProfile>) -> Vec<Agent> {
    profiles.map(|profile| profile.legacy_agent()).collect()
}

#[test]
fn profiles_preserve_exact_identity_order_aliases_and_executables() {
    let registry = registry();
    assert_eq!(registry.known_profiles().len(), EXPECTED_IDENTITIES.len());

    for (profile, (agent, id, aliases, unix_executable, windows_executable)) in
        registry.known_profiles().zip(EXPECTED_IDENTITIES)
    {
        assert_eq!(profile.legacy_agent(), agent);
        assert_eq!(profile.canonical_id(), id);
        assert_eq!(profile.aliases(), aliases);
        assert_eq!(profile.launch().unix, unix_executable);
        assert_eq!(profile.launch().windows, windows_executable);

        #[cfg(not(windows))]
        assert_eq!(profile.launch().executable(), unix_executable);
        #[cfg(windows)]
        assert_eq!(profile.launch().executable(), windows_executable);
    }
}

#[test]
fn canonical_ids_and_lookup_names_are_unique() {
    let registry = registry();

    for (index, profile) in registry.known_profiles().enumerate() {
        assert!(!profile.canonical_id().is_empty());
        assert!(registry
            .profile_by_agent(profile.legacy_agent())
            .is_some_and(|candidate| std::ptr::eq(candidate, profile)));
        assert!(registry
            .profile_by_id(profile.canonical_id())
            .is_some_and(|candidate| std::ptr::eq(candidate, profile)));
        assert!(registry
            .profile_by_normalized_alias(profile.canonical_id())
            .is_some_and(|candidate| std::ptr::eq(candidate, profile)));

        for other in registry.known_profiles().skip(index + 1) {
            assert_ne!(profile.canonical_id(), other.canonical_id());
            assert_ne!(profile.legacy_agent(), other.legacy_agent());
        }

        for (alias_index, alias) in profile.aliases().iter().enumerate() {
            assert!(!alias.is_empty());
            assert!(registry
                .profile_by_normalized_alias(alias)
                .is_some_and(|candidate| std::ptr::eq(candidate, profile)));
            assert!(registry.profile_by_id(alias).is_none());
            assert!(registry
                .known_profiles()
                .all(|other| other.canonical_id() != *alias));

            for other_alias in profile.aliases().iter().skip(alias_index + 1) {
                assert_ne!(alias, other_alias);
            }
            for other in registry.known_profiles().skip(index + 1) {
                assert!(!other.aliases().contains(alias));
            }
        }
    }

    assert!(registry.profile_by_id("Claude").is_none());
    assert!(registry.profile_by_normalized_alias(" claude ").is_none());
    assert!(registry.profile_by_normalized_alias("CLAUDE").is_none());
    assert!(registry.profile_by_normalized_alias("unknown").is_none());
}

#[test]
fn report_policy_matrix_preserves_exact_official_pairs_and_replacement_rules() {
    const EVENTS: [Option<&str>; 10] = [
        None,
        Some("startup"),
        Some("clear"),
        Some("resume"),
        Some("compact"),
        Some("branch"),
        Some("new"),
        Some("fork"),
        Some("select"),
        Some("other"),
    ];

    let registry = registry();
    for (
        agent,
        source,
        authority,
        reserved_native_state,
        replacement_events,
        allows_missing_event,
        unsequenced_event,
        initial_lifecycle_replacement,
    ) in EXPECTED_REPORT_POLICIES
    {
        let profile = registry.profile_for_agent(agent);
        let report = profile.report();
        assert_eq!(report.official_source(), source, "{agent:?}");
        assert_eq!(report.authority(), authority, "{agent:?}");
        assert_eq!(
            report.reserves_native_state(),
            reserved_native_state,
            "{agent:?}"
        );
        assert_eq!(
            report.initial_lifecycle_report_replaces_session(),
            initial_lifecycle_replacement,
            "{agent:?}"
        );

        if let Some(source) = source {
            let id = profile.canonical_id();
            assert_eq!(
                registry.has_full_lifecycle_report_authority(source, id),
                authority == ReportAuthority::FullLifecycle,
                "{agent:?}"
            );
            assert_eq!(
                registry.is_session_identity_only_integration(source, id),
                authority == ReportAuthority::SessionIdentityOnly,
                "{agent:?}"
            );
            assert_eq!(
                registry.is_reserved_native_state_source(source, id),
                reserved_native_state,
                "{agent:?}"
            );
            assert_eq!(
                registry.initial_lifecycle_report_replaces_session(source, id),
                initial_lifecycle_replacement,
                "{agent:?}"
            );
            for event in EVENTS {
                let expected_replacement = match event {
                    Some(event) => replacement_events.contains(&event),
                    None => allows_missing_event,
                };
                assert_eq!(
                    registry.session_report_allows_replacement(source, id, event),
                    expected_replacement,
                    "{agent:?} {event:?}"
                );
                assert_eq!(
                    registry.session_replacement_allows_unsequenced_report(source, id, event),
                    event.is_some_and(|event| unsequenced_event == Some(event)),
                    "{agent:?} {event:?}"
                );
            }

            assert!(!registry.has_full_lifecycle_report_authority("herdr:custom", id));
            assert!(!registry.is_session_identity_only_integration("herdr:custom", id));
            assert!(!registry.is_reserved_native_state_source("herdr:custom", id));
            assert!(!registry.session_report_allows_replacement(
                "herdr:custom",
                id,
                Some("startup")
            ));
            for alias in profile.aliases() {
                assert!(!registry.has_full_lifecycle_report_authority(source, alias));
                assert!(!registry.is_session_identity_only_integration(source, alias));
                assert!(!registry.is_reserved_native_state_source(source, alias));
                assert!(!registry.session_report_allows_replacement(
                    source,
                    alias,
                    Some("startup")
                ));
            }
        }
    }

    assert!(!registry.has_full_lifecycle_report_authority("herdr:pi", "custom-agent"));
    assert!(!registry.session_report_allows_replacement(
        "herdr:opencode",
        "open-code",
        Some("select")
    ));
}

#[test]
fn process_matchers_have_unique_profile_ownership() {
    const EXPECTED_SPECIAL_MATCHERS: [(Agent, usize, bool, bool); 5] = [
        (Agent::Pi, 2, false, false),
        (Agent::Cursor, 0, true, false),
        (Agent::Cline, 0, false, true),
        (Agent::Mastracode, 1, false, false),
        (Agent::Qwen, 1, false, true),
    ];

    let registry = registry();
    let mut actual_special_matchers = Vec::new();

    for profile in registry
        .known_profiles()
        .filter(|profile| profile.process().is_some())
    {
        let process = profile.process().expect("process capability should exist");
        for process_name in std::iter::once(profile.canonical_id())
            .chain(profile.aliases().iter().map(String::as_str))
        {
            assert!(registry
                .profile_by_normalized_process_name(process_name)
                .is_some_and(|owner| std::ptr::eq(owner, profile)));
        }

        let package_layout_count = process.known_package_layouts().len();
        for layout in process.known_package_layouts() {
            assert!(!layout.components().is_empty());
            assert!(layout
                .components()
                .iter()
                .all(|component| !component.is_empty()));
            assert_eq!(
                registry
                    .process_profiles_with_package_layouts()
                    .flat_map(|candidate| {
                        candidate
                            .process()
                            .into_iter()
                            .flat_map(|candidate_process| candidate_process.known_package_layouts())
                    })
                    .filter(|candidate| candidate.components() == layout.components())
                    .count(),
                1
            );
        }
        let has_bundled_node_layout = process.bundled_node_layout().is_some();
        let uses_secondary_fallback = process.uses_secondary_runtime_argv_fallback();
        if package_layout_count > 0 || has_bundled_node_layout || uses_secondary_fallback {
            actual_special_matchers.push((
                profile.legacy_agent(),
                package_layout_count,
                has_bundled_node_layout,
                uses_secondary_fallback,
            ));
        }
    }

    assert_eq!(actual_special_matchers, EXPECTED_SPECIAL_MATCHERS);
    assert_eq!(
        profile_agents(registry.process_profiles_with_package_layouts()),
        [Agent::Pi, Agent::Mastracode, Agent::Qwen]
    );
    assert_eq!(
        profile_agents(registry.process_profiles_with_bundled_node_layout()),
        [Agent::Cursor]
    );
}

#[test]
fn capability_views_preserve_exact_independent_membership() {
    let registry = registry();
    let expected_known: Vec<Agent> = EXPECTED_IDENTITIES
        .iter()
        .map(|(agent, ..)| *agent)
        .collect();

    assert_eq!(profile_agents(registry.known_profiles()), expected_known);
    assert_eq!(profile_agents(registry.known_profiles()), Agent::ALL);
    for agent in Agent::ALL {
        assert_eq!(registry.profile_for_agent(agent).legacy_agent(), agent);
    }
    assert_eq!(
        profile_agents(
            registry
                .known_profiles()
                .filter(|profile| profile.is_startable())
        ),
        expected_known
    );
    assert_eq!(
        profile_agents(
            registry
                .known_profiles()
                .filter(|profile| profile.process().is_some())
        ),
        expected_known
    );
    assert_eq!(
        profile_agents(registry.screen_detectable_profiles()),
        EXPECTED_SCREEN_DETECTABLE
    );
    assert_eq!(
        profile_agents(
            registry
                .known_profiles()
                .filter(|profile| profile.session().is_some())
        ),
        EXPECTED_RESUMABLE
    );
    for profile in registry.known_profiles() {
        let expected_resumable = EXPECTED_RESUMABLE.contains(&profile.legacy_agent());
        assert_eq!(profile.session().is_some(), expected_resumable);
    }
    assert_eq!(
        profile_agents(registry.integration_capable_profiles()),
        EXPECTED_INTEGRATION_CAPABLE
    );
}

#[test]
fn integration_profiles_preserve_exact_metadata_order_and_round_trips() {
    let registry = registry();
    let expected_targets = EXPECTED_INTEGRATION_PROFILES.map(|(target, ..)| target);
    assert_eq!(IntegrationTarget::ALL, expected_targets);
    for (discriminant, target) in IntegrationTarget::ALL.into_iter().enumerate() {
        assert_eq!(target as usize, discriminant);
    }
    assert_eq!(
        profile_agents(registry.integration_capable_profiles()),
        EXPECTED_INTEGRATION_CAPABLE
    );

    for (
        target,
        agent,
        cli_label,
        cli_aliases,
        unix_commands,
        windows_commands,
        unix_version,
        windows_version,
    ) in EXPECTED_INTEGRATION_PROFILES
    {
        let profile = registry.profile_for_agent(agent);
        let integration = profile.integration().expect("integration profile");
        assert_eq!(integration.target(), target);
        assert_eq!(integration.cli_label(), cli_label);
        assert_eq!(integration.cli_aliases(), cli_aliases);
        assert_eq!(integration.definition.commands.unix, unix_commands);
        assert_eq!(integration.definition.commands.windows, windows_commands);
        assert!(integration.definition.supported.unix);
        assert!(integration.definition.supported.windows);
        assert_eq!(integration.definition.versions.unix, unix_version);
        assert_eq!(integration.definition.versions.windows, windows_version);

        #[cfg(not(windows))]
        {
            assert_eq!(integration.command_names(), unix_commands);
            assert!(integration.supported());
            assert_eq!(integration.expected_version(), unix_version);
        }
        #[cfg(windows)]
        {
            assert_eq!(integration.command_names(), windows_commands);
            assert!(integration.supported());
            assert_eq!(integration.expected_version(), windows_version);
        }

        assert!(registry
            .profile_by_integration_target(target)
            .is_some_and(|candidate| std::ptr::eq(candidate, profile)));
        assert!(registry
            .profile_by_integration_cli_name(cli_label)
            .is_some_and(|candidate| std::ptr::eq(candidate, profile)));
        for alias in cli_aliases {
            assert!(registry
                .profile_by_integration_cli_name(alias)
                .is_some_and(|candidate| std::ptr::eq(candidate, profile)));
            assert_eq!(
                registry
                    .integration_capable_profiles()
                    .filter(|candidate| {
                        candidate.integration().is_some_and(|candidate| {
                            candidate.cli_label() == *alias
                                || candidate.cli_aliases().iter().any(|name| name == alias)
                        })
                    })
                    .count(),
                1
            );
        }
        assert_eq!(
            registry
                .integration_capable_profiles()
                .filter(|candidate| {
                    candidate.integration().is_some_and(|candidate| {
                        candidate.cli_label() == cli_label
                            || candidate.cli_aliases().iter().any(|name| name == cli_label)
                    })
                })
                .count(),
            1
        );
        assert_eq!(
            registry
                .known_profiles()
                .filter(|candidate| {
                    candidate
                        .integration()
                        .is_some_and(|candidate| candidate.target() == target)
                })
                .count(),
            1
        );
    }
}

#[test]
fn integration_cli_lookup_is_exact_and_rejects_agent_only_names() {
    let registry = registry();
    for rejected in [
        "Antigravity-cli",
        "antigravity cli",
        " antigravity-cli ",
        "agy",
        "antigravity",
        "kilo-code",
        "unknown",
        "",
    ] {
        assert!(
            registry.profile_by_integration_cli_name(rejected).is_none(),
            "unexpected integration CLI alias: {rejected}"
        );
    }

    assert_eq!(
        registry
            .profile_by_integration_cli_name("antigravity_cli")
            .and_then(|profile| profile.integration())
            .map(|integration| integration.target()),
        Some(IntegrationTarget::AntigravityCli)
    );
}
