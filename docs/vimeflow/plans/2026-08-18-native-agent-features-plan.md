# Native Agent Features — Scope 2 Plan

**Spec:** `../specs/2026-08-18-native-agent-features-design.md` (v2 — read it first)
**Review applied:** `../reviews/2026-08-18-scope2-review.md` (all HIGH + MEDIUM + LOW)
**Repos:** this fork (`winoooops/vimeflow-terminal`, branch `main`) and
`~/projects/agent-watcher` (prerequisite task W only)
**Discipline:** conventional commits, granular; every upstream-file edit gets
the §4(b) notice + a FORK.md registry row in the same commit. Stop and report
on ambiguity — do not guess.

## Blocking input (operator decision, before Task 5/6)

The title-sync TS repo working tree is dirty: uncommitted changes make
`agent_session.agent` sufficient for agent-exit/sync handling
(`src/watcher.ts`, `index.test.ts`). **Operator decides:** commit them in the
TS repo (port source pins that commit) or exclude them (pin `c48327ee`).
Record the chosen commit hash here before starting Task 5.

> Port source commit: `________` (fill in)

## Task W — watcher embedding API (in `~/projects/agent-watcher`, FIRST)

Implements spec §3.0. Work in the agent-watcher repo on a branch; do not touch
`src/agent/**` (PORT-SURFACE.md frozen).

1. `DaemonOptions` (explicit state dir; every path currently derived from
   `HERDR_PLUGIN_STATE_DIR`/`temp_dir()` — singleton lock + control socket,
   state socket, Kimi consent, daemon data — flows from the options).
   `run()` becomes the env-deriving standalone wrapper. Unit test: two
   different options roots produce fully disjoint path sets.
2. Service handle: `start(options) -> Handle { shutdown(), join() }`. The
   singleton control listener, state-server listener + connection workers,
   and sweeper observe the shutdown signal; sockets removed on stop. Binary
   wrapper waits on the handle. Test: **start → stop → start again in one
   process** succeeds with no leaked sockets/threads (the review's exact
   gap).
3. Bridge-install API: script generation takes executable path + argument
   prefix + state path explicitly; the existing CLI passes
   `current_exe()` + `claude-bridge` to keep plugin behavior identical.
   Test: generated script content for a custom prefix.
4. `cargo test` + clippy green; tag **v0.2.0** (includes the post-v0.1.8
   Kimi resume fix already on main); push tag. Record the tag commit here.

## Task 0 — adopt the established build baseline (fork)

CI is already green (upstream Zig setup + nextest, commits `6be9ce52`,
`e0cad0db`, `e312ccf8`, `db4809c7`). Remaining local ergonomics:

1. Script the local Zig cache pre-seed (`scripts/` or a `just` recipe) that
   fetches the themes tarball with curl and seeds Zig's package cache, so
   `cargo build --locked` works from a cold cache; document it in FORK.md
   next to the baseline table.
2. Document the local test convention (`cargo nextest run` per upstream) so
   nobody chases the two known raw-`cargo test` upstream issues.

## Task 1 — link the watcher crate (fork)

1. `[target.'cfg(unix)'.dependencies]` git dep pinned to `v0.2.0`,
   `features = ["runtime"]`.
2. `nix/package.nix`: add the git dependency's `cargoLock.outputHashes`
   entry (repo policy); verify `nix build`.
3. Verify the Windows path still type-checks (`cargo check` with the
   Windows-relevant cfg, per upstream's Windows CI job expectations) — all
   integration behind `cfg(unix)`.
4. Smoke test: a unix-gated fork unit test constructs a watcher type.

## Task 2 — additive config sections (fork)

`[agent_watcher] enabled` (default true); `[title_sync] enabled` (default
true) + `interval_ms` (default 1000, positive). **Startup-only semantics**
(spec §5): register both sections in the parser's known top-level list so
live reload does not misdiagnose; unknown keys inside the sections reported,
not fatal. Tests: defaults, overrides, invalid values, reload-diagnostics
silence.

## Task 3 — embed the daemon (fork)

1. One lifecycle owner used by **both** server constructors — normal startup
   and handoff import in `src/server/headless.rs` (upstream edits: notice +
   registry). Start via the Task W API with
   `state_dir = plugin_paths::plugin_state_dir("herdr-agent-watcher")`,
   **after plugin startup hooks**; stop via the service handle on both exit
   paths.
2. Tests: start-stop on both paths; coexistence ordering (embedded started
   after hooks supersedes a plugin daemon — use the singleton's takeover
   semantics with two options-built daemons in-process); panic isolation
   (daemon panic logged, server loop unaffected).

## Task 4 — `watcher` CLI (fork)

Implement the spec §3.3 contract table exactly (not 1:1 with the plugin):
`status` (new, reads state socket), `doctor`, `sidebar`,
`claude-bridge enable/disable` (bridge-install API, executable = fork binary,
prefix `watcher claude-bridge`), `kimi-consent`. Unix-gated; Windows returns
unsupported-platform. CLI dispatch touchpoint = upstream edit: notice +
registry. **Contract test executes a generated bridge script against the fork
CLI** and asserts it resolves (the review's silent-nonexistent-command trap).

## Task 5 — title-sync policy port (fork, needs the pinned source commit)

Pure policy module (`src/title_sync/policy.rs` or similar):
`renameDecision` + ownership model + per-pane JSON persistence (atomic
rename). Port every `index.test.ts` case at the pinned commit as Rust table
tests.

## Task 6 — orchestration + readers port (fork)

Normative source set **includes `src/adapter/index.ts`** (orchestration
policy) plus the four readers and `utils.ts`, at the pinned commit:

1. Orchestration: agent-identity fallback (`pane.agent` →
   `agent_session.agent`), id-kind gate, reader-failure fallback to terminal
   title, decoration stripping, generic/cwd rejection — with its tests.
2. Readers: claude, codex (incl. resume-process fallback), kimi, opencode
   (**`opencode db path` cached discovery first, then platform defaults**).
3. `rusqlite` as a **direct** unix-gated fork dependency (spec HIGH-5
   resolution); nix hash updated if needed.
4. Fixture-based tests per reader mirroring the TS repo's formats.

## Task 7 — title-sync engine (fork)

1. Extract the **App-level label mutation helper**; route
   `handle_pane_rename` and title-sync through it (upstream edit: notice +
   registry). Persistence + outward events defined once.
2. Direct trigger at the session-mutation point in `app::actions` **before**
   the `effective_state_change?` early return (session-ref-only changes emit
   no pane update); plus lifecycle triggers; plus the `interval_ms` tick.
3. Single coalesced background worker for blocking resolution: snapshot on
   the server thread → resolve off-loop → apply via one App event with a
   pane/session identity re-check.
4. Integration test with fake agent state files: agent start → label set;
   manual rename → never overwritten; agent exit → owned label cleared,
   manual kept; session-ref-only change → recompute fires.

## Task 8 — coexistence, docs, close-out (fork)

1. `watcher doctor` + server startup log warn when a standalone twin plugin
   is linked+enabled alongside its built-in twin, naming the disable command.
2. User-facing docs under **`docs/next/`** per repo convention (AGENTS.md) —
   not the root README; fork engineering notes stay in `docs/vimeflow/`.
3. FORK.md registry complete; build/nextest/clippy green locally; CI green;
   final report: upstream files touched, test counts, operator
   live-verification checklist (fork serves a real agent pane title, watcher
   sidebar opens, doctor warns with plugins still linked, `nix build` ok).

## Execution order

**W first** (its tag gates T1). T0 anytime. Then T1→T2→T3→T4 (watcher
track), T5→T6→T7 (title track — needs the pinned source commit; may
interleave), T8 last. Stop after W and after T0 with short reports.
