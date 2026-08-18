# Native Agent Features — Scope 2 Design

**Date:** 2026-08-18
**Status:** Approved direction (operator, 2026-08-18); pending one codex review pass
**Parent:** vimeflow repo `docs/superpowers/plans/2026-08-18-herdr-fork-bootstrap.md` (Scope 2)
and `docs/superpowers/specs/2026-08-10-herdr-engine-pivot-design.md` (tiers, licensing)

## 1. Summary

The fork binary ships the functionality of two standalone plugins **built in**,
so fork users install nothing:

- **agent-watcher** (`winoooops/herdr-agent-watcher`, Rust, v0.1.8) — coding-agent
  observability: transcript watchers, lifecycle, metrics, notifications, ratatui
  sidebar view.
- **herdr-agent-title-sync** (`winoooops/herdr-agent-title-sync`, TypeScript) —
  pane labels auto-follow agent session titles, manual rename always wins.

The standalone plugin repos **stay alive** for stock-herdr users (pivot spec
Tier-1 decision stands). The fork embeds; the repos continue.

## 2. Non-goals

- The 3-section sidebar chrome (Spaces/Agents/Watcher) — M1.
- The Dynamic Island tab control — M2 (`~/projects/herdr-dynamic-island/SPEC.md`).
- The Tier-2 typed telemetry protocol (`agent.telemetry.report`) — later.
- Branding rename — separate task per FORK.md's rename-surface section.
- Embedding a Node runtime — title-sync is **ported**, not executed as JS.

## 3. agent-watcher: embed as a crate

- **Dependency:** Cargo git dependency pinned to a tag:
  `herdr-agent-watcher = { git = "https://github.com/winoooops/herdr-agent-watcher", tag = "v0.1.8", features = ["runtime"] }`
  (lib name `herdr_agent_watcher`; `runtime` is its default feature). Upgrades =
  bump the pin; the standalone repo remains canonical. No vendored copy, no
  submodule.
- **Process model:** the watcher daemon runs **in-process** — a thread (group)
  started with the server when enabled. It reuses the crate's existing daemon
  entry and keeps the **same singleton lock, state dir, and state socket** as
  the standalone plugin. The existing singleton takeover semantics arbitrate:
  at most one watcher daemon per machine; the embedded one claims/supersedes.
  The state socket stays wire-identical so the sidebar TUI and future Tier-3
  chrome read the same stream.
- **CLI:** a new additive `watcher` subcommand group on the fork binary maps
  the plugin's actions 1:1 — `status`, `doctor`, `enable-claude-bridge`,
  `disable-claude-bridge`, `kimi-consent <on|off|status>`, `sidebar` (runs the
  crate's ratatui sidebar in the current terminal; users open it in any pane).
  Kimi usage consent and the Claude settings bridge remain **explicit opt-in**
  exactly as in the plugin — embedding must not weaken either consent gate.
- **Shutdown:** server shutdown stops the daemon thread cleanly (the crate's
  existing stop path).

## 4. title-sync: port to Rust, in-core

- **Policy module (pure):** port `watcher.ts::renameDecision` and the
  label-ownership model verbatim — a nonempty label differing from the last
  plugin-owned title is manual and always wins; agent exit clears only
  plugin-owned labels; session change with a stale owned label clears. Port
  the TS unit tests (`index.test.ts`) as Rust table tests. Ownership records
  persist as per-pane JSON in the fork's state dir (same schema as the plugin:
  `{ session, title }`), atomic rename writes.
- **Title readers (4):** port from `src/adapter/*.ts`:
  - Claude: `ai-title` / `custom-title` transcript rows, then terminal title.
  - Codex: `~/.codex/session_index.jsonl` `thread_name`, with the exact
    `codex resume <session-id>` process fallback.
  - Kimi: session `state.json` title, then first user prompt.
  - OpenCode: session title from `opencode.db` (rusqlite — already in the
    dependency tree via agent-watcher's runtime feature), then terminal title.
  Honor the same env overrides: `CLAUDE_CONFIG_DIR`, `CODEX_HOME`,
  `KIMI_CODE_HOME`, `OPENCODE_DB_PATH`, XDG vars. Agents without a reader use
  their terminal title.
- **Engine:** in-core, two triggers —
  1. **event-driven:** recompute on internal session-binding / agent / pane
     lifecycle changes (the server owns this state; no socket, no CLI spawns);
  2. **periodic tick** (default 1s, configurable) for title-text changes that
     produce no herdr event (the reason the TS plugin polled). A `notify`-based
     file-watch upgrade is allowed but not required in this scope.
  Renames go through the server's internal pane-label path directly.

## 5. Config (additive)

```toml
[agent_watcher]
enabled = true          # kill switch for the embedded daemon

[title_sync]
enabled = true
interval_ms = 1000      # periodic recompute floor
```

Defaults ON — built-in batteries are the fork's point. Parsing follows the
fork's existing config diagnostics conventions (unknown keys reported, not
fatal).

## 6. Coexistence with the standalone plugins

On the operator's machine both standalone plugins are installed. Rules:

- Watcher daemons: the shared singleton lock arbitrates (embedded supersedes).
- Title watchers: the embedded engine and the plugin's Node watcher would
  fight over labels. `watcher doctor` (and server startup log) **warns** when
  either standalone plugin is linked+enabled while its built-in twin is
  enabled, naming the exact disable command. Docs instruct fork users to
  uninstall/disable the standalone plugins. No automatic disabling of user
  plugins.

## 7. Compatibility invariants

- The `herdr` CLI name, socket path, config path, plugin API, and the watcher
  state-socket wire format are unchanged.
- Upstream-file edits are minimized and every one lands in FORK.md's registry
  with the §4(b) in-file notice. Expected touch set: server startup hook,
  config schema/diagnostics, CLI dispatch. Everything else is additive
  modules.

## 8. Risks

| Risk | Mitigation |
| --- | --- |
| Baseline build is broken (libghostty-vt Zig theme fetch HTTP 400 from clean v0.8.0) | Plan Task 0 fixes/vendors it before any feature work; CI green is the gate |
| Embedded daemon panics take down the server | daemon thread(s) isolated; panics caught and logged, watcher restarts or stays down without killing the server |
| Git-dep pin drifts from standalone repo | FORK.md records the pin; bumping is a deliberate reviewed change |
| Label fights with the plugin's Node watcher | §6 doctor warning + docs; singleton where a lock exists |
| Upstream merge surface grows | additive modules; registry discipline |
