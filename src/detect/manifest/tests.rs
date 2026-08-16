use super::*;

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
fn known_agent_no_match_defaults_to_idle_fallback() {
    let explain = explain(Agent::Codex, "ordinary prompt text");

    assert_eq!(explain.state, AgentState::Idle);
    assert!(!explain.visible_idle);
    assert_eq!(
        explain.fallback_reason.as_deref(),
        Some(DEFAULT_KNOWN_AGENT_IDLE_FALLBACK)
    );
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

        assert_eq!(explain.state, AgentState::Idle);
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
fn minimax_manifest_detects_idle_working_and_blocked_states() {
    // Working (thinking): the "Loading" footer replaces the input box.
    // All three markers — "Ctrl+X steer", "Ctrl+O details", "Esc stop" —
    // are present together only in this sub-state.
    let working_thinking = explain(
        Agent::MiniMax,
        "    └ Completed in 7s · ⚡ 105.2 tok/s\n\n   › list the contents of the /etc directory\n\n  └ • Thinking…\n\n    ⠸ Loading 1s · Enter queue · Ctrl+X steer · Ctrl+O details · Esc stop\n  ────────────────────────────────────────────────────────────────────────\n  ›\n  ────────────────────────────────────────────────────────────────────────\n\n  ~ │ ◇ … │ Ask │ ✦ MiniMax-M3 · Thinking On",
    );
    assert_eq!(working_thinking.state, AgentState::Working);
    assert!(working_thinking.visible_working);
    assert_eq!(
        working_thinking
            .matched_rule
            .as_ref()
            .map(|rule| rule.id.as_str()),
        Some("working_active_turn_footer")
    );

    // Working (running tool): the "Running" footer omits the steer/
    // details pair but still carries "Enter queue" + "Esc stop". The
    // rule must match this sub-state too — otherwise the agent
    // silently regresses to "idle" while a tool is in flight (the
    // input box is still visible, so the prompt_box_idle rule would
    // otherwise win).
    let working_running = explain(
        Agent::MiniMax,
        "  └ • Running  date · 1 output line\n\n    ⠸ Running 7s · ⚡ ~278.4 tok/s · Enter queue · Esc stop\n  ────────────────────────────────────────────────────────────────────────\n  ›\n  ────────────────────────────────────────────────────────────────────────\n\n  ~ │ ◇ … │ Ask │ ✦ MiniMax-M3 · Thinking On",
    );
    assert_eq!(working_running.state, AgentState::Working);
    assert_eq!(
        working_running
            .matched_rule
            .as_ref()
            .map(|rule| rule.id.as_str()),
        Some("working_active_turn_footer")
    );

    // Idle: empty prompt box between the two horizontal rules.
    let idle = explain(
        Agent::MiniMax,
        "    VirtualBox guest artifacts\n    - 7× .vboxclient-*.pid files\n\n    Anything specific you want to dig into?\n\n  ↓ End to latest · ↑ 75 earlier rows · PgUp · ↓ 1 newer row · PgDn\n\n    Message · Enter send · Shift+Enter newline\n  ────────────────────────────────────────────────────────────────────────\n  ›\n  ────────────────────────────────────────────────────────────────────────\n\n  ~ │ ◇ … │ Ask │ ✦ MiniMax-M3 · Thinking On",
    );
    assert_eq!(idle.state, AgentState::Idle);
    assert!(idle.visible_idle);
    assert_eq!(
        idle.matched_rule.as_ref().map(|rule| rule.id.as_str()),
        Some("prompt_box_idle")
    );

    // User composing input inside the prompt box; still idle.
    let idle_composing = explain(
        Agent::MiniMax,
        "    Want me to add that for you?\n\n  ↓ End to latest · ↑ 42 earlier rows · PgUp · ↓ 1 newer row · PgDn\n\n    Message · Enter send · Shift+Enter newline\n  ────────────────────────────────────────────────────────────────────────\n  ›  list files in\n  ────────────────────────────────────────────────────────────────────────\n\n  ~ │ ◇ … │ Ask │ ✦ MiniMax-M3 · Thinking On",
    );
    assert_eq!(idle_composing.state, AgentState::Idle);
    assert!(idle_composing.visible_idle);

    // Historical completion lines in the scrollback must not regress to
    // working state when the active-turn footer is absent.
    let historical = explain(
        Agent::MiniMax,
        "  ├ • Thought for 4.5s\n  └ • Running  cat /etc/passwd",
    );
    assert_eq!(historical.state, AgentState::Idle);
    assert!(!historical.visible_working);

    // Blocked: the permission request dialog appears when the user is in
    // a permission mode stricter than "Full access" (i.e. "Ask" or
    // "Auto"). The dialog is structured as a centered "◆ Approval
    // needed" header followed by a boxed action card with three options
    // (1 Allow for this conversation / 2 Always allow this action / 3
    // Deny) and navigation hints ("enter confirm · esc deny"). The
    // action label (e.g. "Bash", "Read") and question ("Run this
    // command?" / "Allow read?") vary by action type, but the option
    // labels and nav hints are stable across all action types.
    let blocked = explain(
        Agent::MiniMax,
        "   › show me the contents of /etc/passwd\n\n  ├ • Thought for 4.5s\n  └ • Running  cat /etc/passwd\n\n  ◆ Approval needed  bash\n\n  │ Approval needed                                                                     Bash\n  │ Run this command?\n  │   $ {\"command\":\"cat /etc/passwd\"}\n  │   Reason: Needs confirmation: filesystem path is outside the workspace or configured\n  │   allow paths. Path: /etc/passwd\n  │\n  │ → 1 Allow for this conversation  Ask again in a new conversation\n  │   2 Always allow this action     Review and save the exact scope\n  │   3 Deny                         Optionally tell MCode what to do instead\n  │\n  │ ↑/↓ select · 1/2/3 choose · enter confirm · esc deny · ctrl+c stop\n\n  ~ │ ◇ … │ Ask │ ✦ MiniMax-M3 · Thinking On",
    );
    assert_eq!(blocked.state, AgentState::Blocked);
    assert!(blocked.visible_blocker);
    assert_eq!(
        blocked.matched_rule.as_ref().map(|rule| rule.id.as_str()),
        Some("permission_prompt_blocked")
    );
}

#[test]
fn minimax_manifest_detects_plan_mode_dialogs() {
    // 1. "Use Plan mode?" — entry confirmation in non-PLAN mode. The
    // agent offers to enter plan mode for a non-trivial task; the user
    // must confirm. Real v0.1.2 capture.
    let plan_confirm = explain(
        Agent::MiniMax,
        "  ● Let me enter Plan Mode so I can investigate the workspace, ask a couple of high-leverage design questions, and hand you a concrete, approval-ready plan first.\n\n  └ • Enterplanmode\n\n    └ Completed in 12s · ⚡ 99.2 tok/s\n\n  ╭─ Use Plan mode? ────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────╮\n  │ Plan mode structures complex tasks before execution.                                                                                                                    │\n  │                                                                                                                                                                         │\n  │ › Continue with plan                                                                                                                                                    │\n  │   Deny                                                                                                                                                                  │\n  ├─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┤\n  │ ↑/↓ select · Enter confirm · Esc deny                                                                                                                                   │\n  ╰─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────╯\n\n  ~ │ ◇ … │ Ask │ ✦ MiniMax-M3 · Thinking On",
    );
    assert_eq!(plan_confirm.state, AgentState::Blocked);
    assert!(plan_confirm.visible_blocker);
    assert_eq!(
        plan_confirm
            .matched_rule
            .as_ref()
            .map(|rule| rule.id.as_str()),
        Some("plan_mode_dialog_blocked")
    );

    // 2. "Ask ─ 0/3 answered" — multi-question clarification dialog in
    // PLAN mode. Real v0.1.2 capture (3 tabs: Worker deliver / Trigger
    // types / Service topolo).
    let ask_multi = explain(
        Agent::MiniMax,
        "  ● The workspace is essentially empty (just 3 unrelated .txt files), so this is a clean greenfield project. Before I write the plan, I need to lock down three high-leverage design decisions that will materially change the architecture.\n\n  ╭─ Ask ──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────── 0/3 answered ─╮\n  │  ● Worker deliver   ○ Trigger types    ○ Service topolo                                                                                                                 │\n  ├─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┤\n  │ How should the scheduler deliver jobs to handlers?                                                                                                                      │\n  │                                                                                                                                                                         │\n  │ › 1  Embedded handlers (Recommended)  Handlers are Spring beans registered with @JobHandler in the same JVM. Scheduler pulls from DB/Redis and invokes locally.         │\n  │   2  HTTP webhook push                Scheduler POSTs job payloads to registered HTTP endpoints.                                                                          │\n  │   3  Both (mixed)                     Default embedded; an explicit webhook handler type POSTs externally.                                                              │\n  │   4  Other…                           Others...                                                                                                                         │\n  ├─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┤\n  │ ↑↓ move · 1-4 select · Enter next · Tab/←/→ questions · Esc cancel                                                                                                      │\n  ╰─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────╯\n\n  ~ │ ◇ … │ PLAN │ Ask │ ✦ MiniMax-M3 · Thinking On",
    );
    assert_eq!(ask_multi.state, AgentState::Blocked);
    assert!(ask_multi.visible_blocker);
    assert_eq!(
        ask_multi.matched_rule.as_ref().map(|rule| rule.id.as_str()),
        Some("plan_mode_dialog_blocked")
    );

    // 3. "Ask ─ 1 of 1" — single-question clarification dialog in PLAN
    // mode. Real v0.1.2 capture. Footer is "Enter send" (vs. "Enter
    // next" in multi-question).
    let ask_single = explain(
        Agent::MiniMax,
        "  ● I'll ask you to make the call.\n\n    └ Completed in 13s · ⚡ 109.9 tok/s\n\n  ╭─ Ask ────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────── 1 of 1 ─╮\n  │ Auth strategy                                                                                                                                                           │\n  │ Which auth strategy should the new Spring Boot 3.5 service use?                                                                                                         │\n  │ Pick one. I'll update the plan to match.                                                                                                                                │\n  │                                                                                                                                                                         │\n  │ › 1  Spring Security + custom JWT          Service issues and validates its own JWTs (HMAC or RSA). No separate auth server. Simpler, fewer moving parts.               │\n  │   2  Spring Authorization Server (OAuth2)  Full OAuth2 Authorization Server with /oauth2/token, registered clients, scopes, grant types. Heavier, but standardized for  │\n  │                                            multi-client / third-party / open-platform use.                                                                              │\n  │   3  Other…                                Others...                                                                                                                    │\n  ├─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┤\n  │ ↑↓ move · 1-3 select · Enter send · Esc cancel                                                                                                                          │\n  ╰─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────╯\n\n  ~ │ ◇ … │ PLAN │ Ask │ ✦ MiniMax-M3 · Thinking On",
    );
    assert_eq!(ask_single.state, AgentState::Blocked);
    assert!(ask_single.visible_blocker);
    assert_eq!(
        ask_single
            .matched_rule
            .as_ref()
            .map(|rule| rule.id.as_str()),
        Some("plan_mode_dialog_blocked")
    );

    // 4. "Plan Review ─ Frozen Runtime snapshot" — plan approval gate
    // shown after the agent writes the plan and calls ExitPlanMode. The
    // "Frozen Runtime snapshot" subtitle is locale-stable chrome. Real
    // v0.1.2 capture.
    let plan_review = explain(
        Agent::MiniMax,
        "  ● The plan is fully written to the canonical file (31.9 KB, 14 numbered sections plus build/run notes). The ExitPlanMode tool keeps returning a queued work must drain error.\n\n  ╭─ Plan Review ───────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────── Frozen Runtime snapshot ─╮\n  │ Lines 1-35 of 648 · wheel/PgUp/PgDn scroll                                                                                                                              │\n  ├─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┤\n  │ Plan ── implementation plan ──                                                                                                                        │\n  │                                                                                                                                                                         │\n  │ Plan complete. What would you like to do?                                                                                                                               │\n  │ › Agree and start implementation                                                                                                                                        │\n  │   Skip for now                                                                                                                                                          │\n  │   Add context to revise                                                                                                                                                 │\n  ├─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┤\n  │ ↑/↓ select · Enter confirm · Esc skip · PgUp/PgDn scroll                                                                                                                │\n  ╰─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────╯\n\n  ~ │ ◇ … │ PLAN │ Ask │ ✦ MiniMax-M3 · Thinking On",
    );
    assert_eq!(plan_review.state, AgentState::Blocked);
    assert!(plan_review.visible_blocker);
    assert_eq!(
        plan_review
            .matched_rule
            .as_ref()
            .map(|rule| rule.id.as_str()),
        Some("plan_mode_dialog_blocked")
    );

    // 5. Sanity: when ONLY a working footer is present (no dialog), the
    // rule must NOT match — that's working, not blocked.
    let working_only = explain(
        Agent::MiniMax,
        "  ⠸ Running 7s · ⚡ ~278.4 tok/s · Enter queue · Esc stop\n  ────────────────────────────────────────────────────────────────────────\n  ›\n  ────────────────────────────────────────────────────────────────────────\n\n  ~ │ ◇ … │ Ask │ ✦ MiniMax-M3 · Thinking On",
    );
    assert_eq!(working_only.state, AgentState::Working);

    // 6. Anti-regression: idle chat text containing bare
    // "answered" (substring of "unanswered") and " of 1 " (a
    // common English phrase) must NOT match the rule. The
    // chrome anchor "ask ─" (Ask followed by U+2500) is the
    // only Ask-related needle, so free-form prose never fires
    // it. The screen ends with the prompt box `›` so the
    // fallback prompt_box_idle rule should win.
    let false_positive_bare = explain(
        Agent::MiniMax,
        "  ● 1 of 1 user reply was unanswered, will follow up tomorrow.

    Message · Enter send · Shift+Enter newline
  ────────────────────────────────────────────────────────────────────────
  ›
  ────────────────────────────────────────────────────────────────────────

  ~ │ ◇ … │ Ask │ ✦ MiniMax-M3 · Thinking On",
    );
    assert_eq!(false_positive_bare.state, AgentState::Idle);
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
                osc_progress: "",
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
                osc_progress: "",
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
id = "grok"
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
    explain_with_input(
        agent,
        DetectionInput {
            screen,
            osc_title,
            osc_progress,
        },
    )
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
    // prompt, the blocked rule at priority 850 beats osc_title_idle at 250.
    let blocker_screen = "do you want to proceed?\n\
        bash command: rm -rf /tmp/test\n\
        ❯ 1. Yes\n   2. No\n\n\
        Esc to cancel · Tab to amend · ctrl+e to explain\n";
    let result = osc_explain(Agent::Claude, blocker_screen, "✳ Claude Code", "");
    assert_eq!(result.state, AgentState::Blocked);
    assert!(result.visible_blocker);
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
fn codex_screen_working_fallback_handles_static_osc_title() {
    let screen = "• I’ll run it and wait for completion.\n\n\
        ◦ Working (1m 16s • esc to interrupt) · 1 background…\n\n\
        › Use /skills to list available skills\n\n\
        gpt-5.6-sol default · /work\n";
    let result = osc_explain(Agent::Codex, screen, "project", "");

    assert_eq!(result.state, AgentState::Working);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("screen_working_fallback")
    );
    assert!(result.visible_working);
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
fn codex_weak_blocker_outranks_working_fallback() {
    let screen = "• Working (4s • esc to interrupt)\n\
        do you want to continue? [y/n]\n\
        › Use /skills to list available skills\n";
    let result = osc_explain(Agent::Codex, screen, "project", "");

    assert_eq!(result.state, AgentState::Blocked);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("weak_blocker")
    );
    assert!(!result.visible_working);
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
fn codex_screen_working_fallback_ignores_stale_and_prompt_text() {
    let screens = [
        "◦ Working (1m 16s • esc to interrupt)\n\
         ■ Conversation interrupted\n\
         › Use /skills to list available skills\n\
         gpt-5.6-sol default · /work\n",
        "› Explain the text ◦ Working (1m 16s • esc to interrupt)\n\
         gpt-5.6-sol default · /work\n",
        "  ◦ Working (1m 16s • esc to interrupt)\n\
         › Use /skills to list available skills\n\
         gpt-5.6-sol default · /work\n",
    ];

    for screen in screens {
        let result = osc_explain(Agent::Codex, screen, "project", "");
        assert_eq!(result.state, AgentState::Idle);
        assert_eq!(
            result.matched_rule.as_ref().map(|r| r.id.as_str()),
            Some("osc_title_idle")
        );
        assert!(result.visible_idle);
        assert!(!result.visible_working);
    }
}

#[test]
fn codex_screen_working_fallback_ignores_interrupted_short_terminal() {
    let screen = "◦ Working (1m 16s • esc to interrupt)\n\
        ■ Conversation interrupted\n\
        ›\n";
    let result = osc_explain(Agent::Codex, screen, "project", "");

    assert_eq!(result.state, AgentState::Idle);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_idle")
    );
    assert!(result.visible_idle);
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
