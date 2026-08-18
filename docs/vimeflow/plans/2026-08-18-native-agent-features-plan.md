# Native Agent Features — Scope 2 Plan

**Spec:** `../specs/2026-08-18-native-agent-features-design.md` (read it first)
**Repo:** this fork (`winoooops/vimeflow-terminal`), branch `main`
**Discipline:** conventional commits, granular; every upstream-file edit gets
the §4(b) notice + a FORK.md registry row in the same commit. Stop and report
on ambiguity or when herdr internals contradict the spec — do not guess.
Reference sources: `~/projects/agent-watcher` (Rust crate),
`~/projects/herdr-agent-title-sync` (TS policy + adapters + tests).

## Task 0 — make `main` build and test green (BLOCKER)

The recorded baseline fails before compiling herdr: the vendored
`libghostty-vt` Zig build gets HTTP 400 for
`ghostty-themes-release-20260629-161812-8c97c3c.tgz`, reproducing on clean
v0.8.0.

1. Diagnose: transient vs dead URL vs Zig fetch-cache issue. Check how
   upstream CI and the Homebrew formula build (both succeeded ≤2 weeks ago) —
   copy their mechanism if they cache/vendor.
2. Fix with the smallest durable change (vendor the tarball, pin a mirror
   URL, or pre-seed the Zig cache in build.rs/CI). If the fix touches an
   upstream file: §4(b) notice + registry row.
3. Update FORK.md's baseline table with the root cause and the now-green
   numbers. Gate: `cargo build` + `cargo test` locally AND fork CI green on
   both OSes.

## Task 1 — link the agent-watcher crate

1. Add the git dependency pinned to `v0.1.8` with `features = ["runtime"]`.
2. Smoke test: a fork unit test constructs a trivial type from
   `herdr_agent_watcher` (e.g. the store) proving the link.
3. If the crate fails to compile inside the fork workspace (toolchain/lints),
   report — do not patch the standalone repo silently.

## Task 2 — additive config sections

`[agent_watcher] enabled` (default true), `[title_sync] enabled` (default
true) + `interval_ms` (default 1000, positive). Wire into the fork's config
parse + diagnostics conventions (unknown keys reported, not fatal). Unit
tests for defaults, overrides, and invalid values.

## Task 3 — embed the watcher daemon

1. Locate the server startup path (`src/main.rs` → server mode) and the
   shutdown path. This is an upstream-file edit: notice + registry.
2. When `[agent_watcher].enabled`, start the crate's daemon on a dedicated
   thread after the server is up, preserving the crate's singleton
   lock/state-dir/state-socket behavior unchanged. Verify by test or by live
   check that a running standalone plugin daemon is superseded (crate's
   existing takeover semantics) and that two embedded starts cannot double-run.
3. Panic isolation: a daemon panic is caught and logged; the server survives.
4. Clean stop on server shutdown.

## Task 4 — `watcher` CLI subcommand group

Additive CLI module mapping the plugin actions 1:1: `status`, `doctor`,
`enable-claude-bridge`, `disable-claude-bridge`,
`kimi-consent <on|off|status>`, `sidebar` (runs the crate's ratatui sidebar
in the current terminal). CLI dispatch touchpoint = upstream edit: notice +
registry. Consent gates behave exactly as in the plugin.

## Task 5 — title-sync policy port (pure)

Port `watcher.ts::renameDecision` + ownership model to a pure Rust module
(new additive file, e.g. `src/title_sync/policy.rs`). Port every case in the
TS repo's `index.test.ts` as Rust table tests, plus the ownership-record
persistence (per-pane JSON `{ session, title }`, atomic rename writes) under
the fork's state dir.

## Task 6 — title readers port (4 adapters)

Port `src/adapter/{claude,codex,kimi,opencode}.ts` + `utils.ts` semantics:
sources, fallbacks, env overrides, and title sanitization per spec §4. Use
rusqlite for opencode. Fixture-based unit tests per reader (craft minimal
transcript/jsonl/state.json/sqlite fixtures mirroring the TS repo's observed
formats).

## Task 7 — title-sync engine in-core

1. Find where the server updates pane agent-session bindings and pane
   lifecycle; hook recompute there (upstream edit: notice + registry).
2. Periodic tick from `[title_sync].interval_ms` for title-text changes.
3. Renames go through the internal pane-label path; policy module decides;
   ownership records persist.
4. Integration test with fake agent state files: agent starts → label set;
   manual rename → never overwritten; agent exits → owned label cleared,
   manual label kept.

## Task 8 — coexistence, docs, close-out

1. `watcher doctor` + server startup log warn when a standalone twin plugin
   is linked+enabled while the built-in is enabled, naming the disable
   command.
2. README (fork) section: built-in agent features, config, migration from the
   standalone plugins.
3. FORK.md registry complete; `cargo build`/`test`/`clippy` green; CI green;
   final report: upstream files touched, test counts, live-verification
   checklist for the operator (start fork, watch a real agent pane get
   titled, open `watcher sidebar`, doctor output with plugins still linked).

## Execution order

T0 strictly first (everything gates on green). T1→T2→T3→T4 (watcher track),
then T5→T6→T7 (title track; T5/T6 may interleave with T3/T4 if convenient),
T8 last. Stop after T0 with a short report before continuing.
