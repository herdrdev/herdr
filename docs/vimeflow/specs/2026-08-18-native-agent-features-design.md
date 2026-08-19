# Native Agent Features — Scope 2 Design

**Date:** 2026-08-18 (v2 2026-08-19 — revised for all findings in
`../reviews/2026-08-18-scope2-review.md`)
**Status:** Approved direction (operator); v2 addresses review HIGH-1..7,
MEDIUM-1..5, LOW-1
**Parent:** vimeflow repo `docs/superpowers/plans/2026-08-18-herdr-fork-bootstrap.md`
(Scope 2) and `docs/superpowers/specs/2026-08-10-herdr-engine-pivot-design.md`

## 1. Summary

The fork binary ships the functionality of two standalone plugins **built in**,
so fork users install nothing:

- **agent-watcher** (`winoooops/herdr-agent-watcher`, Rust) — coding-agent
  observability: transcript watchers, lifecycle, metrics, notifications,
  ratatui sidebar view.
- **herdr-agent-title-sync** (`winoooops/herdr-agent-title-sync`, TypeScript)
  — pane labels auto-follow agent session titles, manual rename always wins.

The standalone plugin repos **stay alive** for stock-herdr users; the fork
embeds, the repos continue.

## 2. Non-goals

- The 3-section sidebar chrome (M1); the Dynamic Island (M2); Tier-2 typed
  telemetry; branding rename.
- Embedding a Node runtime — title-sync is ported, not executed as JS.
- Live config start/stop of the embedded features (see §5 — startup-only in
  this scope; the §3 service handle makes live control possible later).
- Windows support for the embedded features (see §3 platform gating).

## 3. agent-watcher: embed as a crate

### 3.0 Prerequisite: the watcher embedding API (agent-watcher repo)

Review HIGH-1/2/4 established that `v0.1.8` is a process-owned daemon, not an
embeddable service. Before fork work, the **agent-watcher repo** grows an
embedding API, released as a new tag (target **v0.2.0**, which also picks up
the post-`v0.1.8` Kimi resume fix — review MEDIUM-3):

1. **Options-taking entry:** a `DaemonOptions { state_dir, ... }`-style
   constructor where every path the daemon derives from
   `HERDR_PLUGIN_STATE_DIR` / `temp_dir()` today (singleton lock + control
   socket, state socket, Kimi consent, daemon data) comes from the options.
   The existing `run()` becomes the standalone wrapper deriving options from
   its environment. No global env mutation in the fork (HIGH-1).
2. **Service handle:** starting the daemon returns a handle with
   `shutdown()` + `join()`. The singleton control listener, state-server
   listener, connection workers, and sweeper all observe the shutdown signal
   and terminate; sockets are removed on stop. The standalone binary waits on
   the same handle (HIGH-2).
3. **Bridge-install API:** the Claude-bridge script generator accepts the
   executable path, the argument prefix (e.g. `watcher claude-bridge`), and
   the state path explicitly, instead of assuming `current_exe()` +
   plugin-env (HIGH-4).

`src/agent/**` stays frozen per PORT-SURFACE.md; the API work touches
daemon/service plumbing only.

### 3.1 Dependency (fork side)

- Under **`[target.'cfg(unix)'.dependencies]`** (HIGH-3 — the crate's
  `runtime` feature deliberately fails on non-Unix, and upstream CI includes
  Windows):
  `herdr-agent-watcher = { git = "https://github.com/winoooops/herdr-agent-watcher", tag = "v0.2.0", features = ["runtime"] }`
- All watcher integration and CLI code is `cfg(unix)`-gated; on Windows the
  `watcher` CLI returns a clear unsupported-platform error.
- `nix/package.nix` gains the git dependency's `cargoLock.outputHashes` entry
  in the same commit (repo policy, AGENTS.md), verified by `nix build`.
- The pin records the exact watcher commit in FORK.md; bumping is a reviewed
  change.

### 3.2 Process model

- The daemon runs in-process via the §3.0 API, with
  `state_dir = plugin_paths::plugin_state_dir("herdr-agent-watcher")` — the
  exact directory the plugin launcher exports — so singleton lock, state
  socket, consent, and data are **byte-identical locations** to the
  standalone plugin. The existing takeover semantics then genuinely
  arbitrate: at most one daemon per machine.
- **Lifecycle owner (MEDIUM-2):** one owner used by **both** server
  constructors — normal startup and handoff import
  (`src/server/headless.rs`). It starts the daemon **after plugin startup
  hooks** run (so an embedded daemon started last supersedes a
  plugin-launched one, not the reverse) and stops it on both server exit
  paths via the service handle.
- Panic isolation: a daemon panic is caught and logged; the server survives.

### 3.3 CLI (explicit contract — not "1:1")

Additive `watcher` subcommand group (unix; unsupported-platform error
elsewhere):

| Command | Behavior |
| --- | --- |
| `watcher status` | NEW — reads the state socket, prints daemon liveness + bound agents summary |
| `watcher doctor` | crate doctor + §6 coexistence warnings |
| `watcher sidebar` | runs the crate's ratatui sidebar in the current terminal |
| `watcher claude-bridge enable/disable` | via the §3.0 bridge-install API with executable = the fork binary and prefix `watcher claude-bridge`; generated scripts are contract-tested by executing them against the fork CLI |
| `watcher kimi-consent on/off/status` | crate consent API against the §3.2 state dir |

Deliberately **not** ported: `stop` (the server owns the daemon lifecycle),
`sidebar-open`/`bind-sidebar-key`/`unbind-sidebar-key` (plugin-pane and
plugin-keybinding machinery; fork users open `watcher sidebar` in a pane and
bind keys in fork config). Both consent gates (Kimi network usage, Claude
settings bridge) remain explicit opt-in.

## 4. title-sync: port to Rust, in-core

### 4.1 Normative sources (MEDIUM-3)

- Policy + orchestration + tests from `winoooops/herdr-agent-title-sync` at a
  **pinned commit recorded in the plan**. The working tree currently carries
  uncommitted changes altering agent-exit/sync policy — the operator decides:
  commit them (pin includes them) or exclude them (pin `c48327ee`). Blocking
  question, tracked in the plan.

### 4.2 What ports

- **Policy (pure):** `watcher.ts::renameDecision` + ownership model — manual
  label always wins; agent exit clears only plugin-owned labels; session
  change with a stale owned label clears. TS `index.test.ts` cases become
  Rust table tests. Ownership records: per-pane JSON `{ session, title }`,
  atomic rename writes, under the fork's state dir.
- **Orchestration (HIGH-6 — `src/adapter/index.ts` is policy, not a
  barrel):** agent identity falls back `pane.agent` → `agent_session.agent`;
  only `agent_session.kind == "id"` reaches durable readers; reader failures
  fall through to terminal title; terminal-title decoration stripping;
  generic-program-name and cwd-echo rejection. Ported with its tests.
- **Readers (4):** claude (transcript `ai-title`/`custom-title` rows),
  codex (`session_index.jsonl` `thread_name` + exact
  `codex resume <session-id>` process fallback), kimi (`state.json` title,
  first-user-prompt fallback), opencode (**discovery order: cached
  `opencode db path` command first, then platform defaults** — HIGH-6; DB via
  **rusqlite added as a direct fork dependency**, unix-gated — HIGH-5).
  Env overrides as in TS: `CLAUDE_CONFIG_DIR`, `CODEX_HOME`,
  `KIMI_CODE_HOME`, `OPENCODE_DB_PATH`, XDG.

### 4.3 Engine (MEDIUM-1, MEDIUM-5)

- **Label mutation:** extract **one App-level label helper** that owns
  persistence + outward events; both the API rename handler
  (`app::api::panes::handle_pane_rename`) and title-sync route through it.
  No direct `TerminalState::set_manual_label` calls from title-sync.
- **Triggers:** (a) a **direct hook at the session-mutation point** in
  `app::actions` — before the `effective_state_change?` early return, because
  session-ref-only changes emit no pane update; (b) pane/agent lifecycle
  changes; (c) periodic tick (default 1s) for title-text changes that produce
  no event.
- **Execution boundary:** one **coalesced background worker** does the
  blocking work (file scans, `opencode db path`, SQLite). The server thread
  snapshots pane/session inputs → worker resolves off-loop → results come
  back as a single App event that **re-checks pane/session identity** before
  applying. No per-pane timers, no fs-notification layer in this scope.

## 5. Config (additive, startup-only — MEDIUM-4)

```toml
[agent_watcher]
enabled = true

[title_sync]
enabled = true
interval_ms = 1000
```

- Both sections are **read at server startup only**; changing them requires a
  server restart, and the docs say so. (Live start/stop needs the §3.0
  service handle everywhere and is deferred.)
- Task 2 still registers both sections in the config parser's known
  top-level list so live reload **diagnoses nothing spuriously** and unknown
  keys inside the sections are reported, not fatal.

## 6. Coexistence with the standalone plugins

- Watcher daemons: same state dir ⇒ the singleton genuinely arbitrates;
  embedded starts after plugin hooks and supersedes (§3.2).
- Title watchers: the embedded engine and the plugin's Node watcher would
  fight over labels. `watcher doctor` + a server startup log line warn when a
  standalone twin is linked+enabled while its built-in twin is enabled,
  naming the exact disable command. No automatic disabling of user plugins.

## 7. Compatibility invariants

- The `herdr` CLI name, socket path, config path, plugin API, and the watcher
  state-socket wire format are unchanged.
- Upstream-file edits minimized, each with the §4(b) notice + FORK.md
  registry row. Expected touch set: server startup/shutdown (both
  constructors), config schema/known-list, CLI dispatch, the App label
  helper + session-mutation hook. Everything else is additive modules.

## 8. Risks

| Risk | Mitigation |
| --- | --- |
| Watcher API work stalls Scope 2 | It is small, additive, benefits the standalone plugin too; executed first as its own task with its own tag |
| Local cold-cache Zig fetch 400 (Zig client behavior; curl succeeds) | Baseline solved: pre-seed script + FORK.md documentation; CI unaffected |
| Embedded daemon panic takes down the server | catch + log; server survives; daemon stays down until restart |
| Label fights with the plugin's Node watcher | §6 doctor warning + docs; singleton where a lock exists |
| Blocking title readers stall the server | §4.3 single background worker + identity re-check |
| Upstream merge surface grows | additive modules; registry discipline |
