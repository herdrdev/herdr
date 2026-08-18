# Scope 2 native-agent-features review

**Reviewed:** 2026-08-18  
**Spec:** `docs/vimeflow/specs/2026-08-18-native-agent-features-design.md`  
**Plan:** `docs/vimeflow/plans/2026-08-18-native-agent-features-plan.md`  
**Verdict:** **NO-GO as written.** The dependency coordinates are real, but the
watcher daemon is not an embeddable service API, the title-reader port omits
its orchestration layer, and Task 0 understates the current baseline. Resolve
the HIGH findings in the spec/plan before Scope 2 implementation.

This was a review only. No spec, plan, source, config, or dependency file was
changed.

## Claims that do check out

- `agent-watcher/Cargo.toml:1-26` defines package
  `herdr-agent-watcher` version `0.1.8`, library `herdr_agent_watcher`, tag
  `v0.1.8` exists at `d3b709479b847ec42097b27ddcd76da33ca3b325`, and
  `runtime` exists and is the default feature. A local
  `cargo check --lib --features runtime` succeeds on macOS. The dependency is
  therefore valid on a Unix target, subject to HIGH-3 below.
- `agent-watcher/src/main.rs:1-77` owns `env_logger::init()` and
  `process::exit`; `daemon::run::run()` itself does not call `process::exit` or
  install a signal handler. Calling the library entry would not directly exit
  the Herdr server or install a second logger. Its lifecycle and path ownership
  still make it unsuitable for embedding (HIGH-1/HIGH-2).
- The fork can reach session-binding state: `src/app/actions.rs:2793-2821`
  handles hook session updates, and `TerminalState` stores the persisted agent
  session. Codex foreground process data is also available in-core through the
  same `detect::foreground_job` path used by
  `src/app/api/panes.rs:196-224`. The missing pieces are reliable trigger and
  label-mutation boundaries (MEDIUM-1).

## HIGH findings

### HIGH-1 — The embedded and standalone daemons would not share a singleton or state socket

**Spec/plan:** design §3, lines 37-43 and §6, line 99; plan Task 3, lines
47-51.

**What is wrong:** The standalone plugin gets `HERDR_PLUGIN_STATE_DIR` from
the fork's plugin launcher (`src/app/api/plugins/env.rs:15-29`), whose value is
`state_dir()/plugins/<plugin-id>` (`src/plugin_paths.rs:21-25`). The in-core
server does not have that plugin-process environment. At tag `v0.1.8`, the
watcher independently falls back to `std::env::temp_dir()` for:

- singleton lock/control socket (`agent-watcher/src/daemon/singleton.rs:10-22`),
- state socket (`agent-watcher/src/daemon/mod.rs:9-17`),
- Kimi consent (`agent-watcher/src/agents/consent.rs:5-10`), and
- daemon data (`agent-watcher/src/daemon/run.rs:77-80`).

Consequently, a naïve in-process `run()` uses `/tmp`, while the linked plugin
uses Herdr's plugin state directory. They can both run, publish different state
sockets, and read different consent/data. The claimed takeover and wire-state
continuity do not happen. Setting `HERDR_PLUGIN_STATE_DIR` globally inside the
server is not a safe fix: process environment is global and would leak plugin
identity into unrelated in-process work.

**Concrete fix:** First add a canonical watcher-library API that accepts
explicit paths/options. The fork must pass exactly
`plugin_paths::plugin_state_dir("herdr-agent-watcher")` for backward-compatible
singleton, socket, consent, and data locations. Keep `run()` as the standalone
wrapper that derives those options from its environment. Pin Scope 2 to a new
watcher tag containing that API.

### HIGH-2 — The watcher has no clean embedded shutdown path

**Spec/plan:** design §3, lines 50-51; plan Task 3, lines 52-53.

**What is wrong:** `daemon::run::run()` returns only an integer and exposes no
shutdown or join handle (`agent-watcher/src/daemon/run.rs:146-172,203-295`).
The singleton's control listener is an untracked thread blocked on
`listener.incoming()` (`agent-watcher/src/daemon/singleton.rs:67-87`). The
state server similarly owns only a private join handle, has no `Drop`/stop
implementation, and blocks forever on `incoming()`
(`agent-watcher/src/daemon/state_server.rs:15-55`). Dropping these values
detaches their threads; the standalone binary relies on process exit to clean
them up. Catching a panic around the outer daemon thread does not stop the
helpers or remove their sockets.

**Concrete fix:** In the watcher crate, introduce the minimum service handle
needed for embedding: a shared shutdown signal plus a `join`/`shutdown` method.
Make the singleton control listener, state listener, connection workers, and
sweeper observe that signal and terminate. Have the existing binary wrapper
wait on the same handle. Task 3 must test a start-stop-start cycle in one
process, not merely that the reconcile loop returns.

### HIGH-3 — The proposed dependency breaks Windows and omits required Nix lock metadata

**Spec/plan:** design §3, lines 32-36; plan Task 1, lines 28-34.

**What is wrong:** `agent-watcher/src/lib.rs:24-25` deliberately emits a
compile error when `runtime` is enabled on a non-Unix target. The fork's
upstream workflow still includes Windows (`.github/workflows/ci.yml:43-57`),
so an unconditional dependency with `features = ["runtime"]` makes the crate
graph fail before Herdr's Windows gates can help. Separately, the fork's Nix
package consumes `Cargo.lock` (`nix/package.nix:56-58`), and repository policy
explicitly requires `cargoLock.outputHashes` for git dependencies
(`AGENTS.md:239`). Task 1 mentions neither constraint.

**Concrete fix:** Put the runtime git dependency under
`[target.'cfg(unix)'.dependencies]`, gate all watcher integration and commands
with `cfg(unix)`, and define the Windows CLI behavior explicitly (omit it or
return an unsupported-platform error). Add the git source hash to
`nix/package.nix` in the dependency commit and verify Windows check plus
`nix build`.

### HIGH-4 — The planned watcher CLI cannot reuse the Claude bridge API correctly

**Spec/plan:** design §3, lines 44-49; plan Task 4, lines 55-61.

**What is wrong:** The claimed 1:1 action mapping is not the crate's actual
surface. `agent-watcher/src/main.rs:4-72` has no `status`; it does have `stop`,
`sidebar-open`, `bind-sidebar-key`, and `unbind-sidebar-key`, which the plan
omits. More importantly, `cli_enable()` captures
`std::env::current_exe()` (`agent-watcher/src/agents/claude_bridge.rs:622-677`)
and passes it to script generation. The generated scripts invoke that path as
`<binary> claude-bridge ...`
(`agent-watcher/src/agents/bridge_scripts.rs:25-62`). Embedded in the fork,
that becomes `herdr claude-bridge`, but the planned grammar is
`herdr watcher ...`; the bridge silently calls a nonexistent command. These
helpers also derive their state socket from plugin environment, which is absent
in the core CLI (HIGH-1).

**Concrete fix:** Define the desired fork CLI contract explicitly instead of
calling it 1:1. Extend the watcher crate with a bridge-install API that accepts
the executable, argument prefix (for example `watcher claude-bridge`), and
state path, then pin the new tag. Decide whether `status`, stop/open, and
keybinding actions exist and add contract tests that execute the generated
scripts against the fork CLI.

### HIGH-5 — `rusqlite` is not available to the fork through a transitive dependency

**Spec/plan:** design §4, lines 62-70; plan Task 6, lines 71-77.

**What is wrong:** `rusqlite` is an optional direct dependency of
agent-watcher (`agent-watcher/Cargo.toml:16-20,41`), but Rust crates cannot
import a transitive dependency by name. The new fork title-sync module cannot
`use rusqlite` merely because the watcher runtime enables it.

**Concrete fix:** Add `rusqlite` as a direct, target-appropriate fork
dependency with the compatible version/features, or expose the OpenCode lookup
through a small public watcher API and reuse that. Record the chosen dependency
and its platform implications in Task 6; do not rely on transitive visibility.

### HIGH-6 — Task 6 omits the adapter orchestration that defines fallback behavior

**Spec/plan:** design §4, lines 62-78; plan Task 6, lines 71-77.

**What is wrong:** The plan names the four reader files and `utils.ts`, but
omits `src/adapter/index.ts`. That file is not a barrel: it contains essential
policy (`herdr-agent-title-sync/src/adapter/index.ts:47-93`):

- agent identity falls back from `pane.agent` to `agent_session.agent`;
- only `agent_session.kind == "id"` is sent to durable readers;
- reader failures fall through to the terminal title;
- terminal decorations are stripped; and
- generic program names and cwd echoes are rejected.

The current TS integration test now specifically exercises an
`agent_session.agent`-only pane (`index.test.ts:165-214`). Porting only the
listed files produces behavior that is observably incomplete. OpenCode
discovery is also more than `OPENCODE_DB_PATH` plus XDG: it first caches
`opencode db path` and only then uses platform defaults
(`src/adapter/opencode.ts:30-45`).

**Concrete fix:** Add `src/adapter/index.ts` to Task 6's normative source set
and port its tests/semantics explicitly: agent fallback, ID-kind gate, reader
error fallback, terminal cleanup, generic/cwd rejection, and OpenCode command
discovery/platform fallback. Treat the four readers as implementations behind
that orchestration, not as the whole feature.

### HIGH-7 — Task 0 describes one blocker, but the current baseline has three independent failures

**Spec/plan:** design §8, line 120; plan Task 0, lines 11-26.

**What is wrong:** The URL narrative is not sufficient to make the gate green.
Current local evidence is:

```text
$ curl https://deps.files.ghostty.org/ghostty-themes-release-20260629-161812-8c97c3c.tgz
HTTP 200, 74116 bytes
$ zig fetch <same-url>
error: bad HTTP response code: '400 Bad Request'
$ cargo build --locked
vendor/libghostty-vt/build.zig.zon:119:20: error: bad HTTP response code: '400 Bad Request'
```

Thus the local symptom is Zig-fetch-specific, not presently a dead URL. The
latest fork CI run (`32118836125`) shows two additional blockers:

- Ubuntu builds, then raw `cargo test --locked` reports **2942 passed / 20
  failed**. One failure,
  `startup_still_auto_opens_unseen_product_announcement`, contradicts the
  Scope-1 change in `src/product_announcements.rs:82-91` and is a fork
  regression, not an untouched-v0.8.0 baseline. Many later failures are
  poisoned-lock cascades, but the first failures still need characterization.
- macOS does not fail on the theme URL; it fails linking Zig with undefined
  Darwin symbols. The fork workflow uses `mlugg/setup-zig` on macOS
  (`.github/workflows/fork-ci.yml:29-32`), while upstream deliberately uses
  patched Homebrew Zig (`.github/workflows/ci.yml:99-115`).

The fork also runs raw `cargo test`, whereas repository convention and
upstream CI use `just check`/`just ci` with nextest (`AGENTS.md:97-100`,
`.github/workflows/ci.yml:136-143`). A theme tarball change alone cannot meet
Task 0's local-and-both-OS-green gate.

**Concrete fix:** Rewrite Task 0 as a characterization matrix before choosing
a fix: clean `v0.8.0` vs fork `main`, local macOS vs both CI OSes, and upstream
`just ci` vs fork raw Cargo commands. Restore the upstream macOS Zig mechanism;
separate the Zig-fetch issue from test failures; classify each non-cascade
test failure as pre-existing, workflow-induced, flaky, or fork-caused; fix the
Scope-1 announcement regression or update its obsolete test intentionally.
Only then select cache pre-seeding/vendor/mirror work and record green counts.

## MEDIUM findings

### MEDIUM-1 — Session state is reachable, but the claimed internal label/trigger path does not exist

**Spec/plan:** design §4, lines 72-78; plan Task 7, lines 79-88.

**What is wrong:** The only combined pane-label operation is the API handler
`App::handle_pane_rename`, which is `pub(super)` inside `app::api::panes` and
marks the session dirty (`src/app/api/panes.rs:1141-1165`). The only generally
reachable methods are raw `TerminalState::set_manual_label/clear_manual_label`
(`src/terminal/state.rs:1761-1768`), which do not own persistence or event side
effects. There is no reusable "server's internal pane-label path" for a sibling
title-sync module.

Session bindings are reachable, but a session-ref-only mutation marks the
session dirty and then returns no `PaneStateUpdate` because
`effective_state_change` is absent (`src/app/actions.rs:2938-2972`). Therefore
subscribing to existing pane/agent state updates will miss exactly some
session-only changes the event trigger is meant to catch.

**Concrete fix:** Make Task 7 explicitly extract one App-level label mutation
helper and route both API rename and title sync through it, defining persistence
and outward-event behavior once. Add a direct internal title-sync trigger at
the session mutation point before the `effective_state_change?` early return,
rather than assuming a downstream pane update exists.

### MEDIUM-2 — Startup placement is ambiguous and misses the handoff server path

**Spec/plan:** design §3, lines 37-43; plan Task 3, lines 45-53.

**What is wrong:** Normal startup and handoff import are separate flows:
`src/server/headless.rs:4656-4743` and `4776-4845`. Both run plugin startup
hooks immediately before `server.run()` (`4735-4738`, `4837-4840`). "After the
server is up" does not identify an ownership-safe point. If the embedded daemon
claims first and startup hooks then launch the standalone plugin, the
standalone process can supersede the built-in—the reverse of the spec. Hooking
only normal startup also loses the daemon after a handoff import.

**Concrete fix:** Specify one lifecycle owner used by both constructors, with
startup after plugin startup hooks (if embedded must win) and shutdown tied to
both server exits. Add a handoff-path lifecycle test and a coexistence ordering
test.

### MEDIUM-3 — Both reference implementations are unpinned relative to the plan

**Spec/plan:** design §3, lines 32-36 and §4, lines 55-70; plan reference
sources, lines 8-9.

**What is wrong:** Watcher tag `v0.1.8` is real, but local watcher HEAD
`37de8b11` is ahead and includes `fix(kimi): a resumed session binds instead of
blinking`; pinning `v0.1.8` knowingly excludes that current behavior. The TS
repo is referenced only by a mutable path and is currently dirty at
`c48327ee`: uncommitted changes make `agent_session.agent` sufficient for
agent-exit/sync handling in `src/watcher.ts` and `index.test.ts`. "Port
verbatim" has no reproducible source revision.

**Concrete fix:** Record exact source commits in the plan. Release and pin a
new watcher tag if the Kimi fix and embedding API are required. Commit or
otherwise explicitly exclude the TS working-tree fix, then name that commit as
the normative port source.

### MEDIUM-4 — The config "kill switches" have no defined reload semantics

**Spec/plan:** design §5, lines 80-93; plan Tasks 2-3 and 7.

**What is wrong:** The fork supports live config reload
(`src/config/io.rs:218-349`, `src/server/headless.rs:1433`), but the plan only
checks `[agent_watcher].enabled` at server startup and gives no start/stop rule
for either feature after reload. Calling these settings kill switches implies
an operator can turn the feature off; with the Task 3 process shape, changing
the config cannot stop leaked watcher helpers (HIGH-2). The parser also has an
explicit known-top-level list and per-section live loaders
(`src/config/io.rs:7-19,263-342`), so adding fields only to `Config` would still
diagnose/ignore them during live reload.

**Concrete fix:** State whether both sections are startup-only (and document
restart required) or live. If live, Task 2 must add both sections to the known
list/live loader and Task 3/7 must own start/stop/reconfigure handles, with
reload tests for enabled→disabled→enabled and interval changes.

### MEDIUM-5 — Synchronous reader ports need an execution boundary

**Spec/plan:** design §4, lines 72-77; plan Task 7, lines 79-88.

**What is wrong:** The TS readers scan JSONL/transcript files, canonicalize
paths, may launch `opencode db path`, and query SQLite
(`src/adapter/utils.ts:31-59`, `opencode.ts:15-45`, `codex.ts:54-66`). The plan
does not say where this blocking work runs. Running it directly in Herdr's
server action/tick path once per pane can stall input/render/API processing;
running multiple independent timers creates ordering races with ownership
state.

**Concrete fix:** Specify one coalesced background worker for blocking title
resolution. Snapshot pane/session inputs on the server thread, resolve off the
event loop, and apply results back through a single App event after rechecking
pane/session identity. A filesystem notification abstraction is unnecessary
for this scope.

## LOW findings

### LOW-1 — Task 8 targets the wrong documentation location

**Spec/plan:** plan Task 8, lines 90-100.

**What is wrong:** Task 8 says to edit the root README. Repository convention
states that normal feature work stages user-facing docs under
`docs/next/website/src/content/docs/` and `docs/next/README.md`, and should not
edit root `README.md` (`AGENTS.md:162-168`). Fork-local engineering records are
already allowlisted under `docs/vimeflow/` (`.gitignore:10-19`).

**Concrete fix:** Put migration/operator documentation in the appropriate
`docs/next` draft and keep fork engineering notes under `docs/vimeflow/`. Edit
root README only if Scope 2 explicitly overrides the release-doc convention.

## Evidence commands

Representative commands used for this review:

```sh
git show v0.1.8:Cargo.toml
git diff v0.1.8 -- Cargo.toml src/lib.rs src/main.rs src/daemon/
cargo check --lib --features runtime                 # in agent-watcher
rg -n 'HERDR_PLUGIN_STATE_DIR|current_exe|process::exit' src
rg -n 'handle_pane_rename|set_manual_label|session_ref_changed' src
git status --short && git diff                       # in title-sync
cargo build --locked                                 # in vimeflow-terminal
gh run view 32118836125 --repo winoooops/vimeflow-terminal
gh run view --job 95654381619 --log                  # Ubuntu
gh run view --job 95654381889 --log                  # macOS
curl -L https://deps.files.ghostty.org/ghostty-themes-release-20260629-161812-8c97c3c.tgz
zig fetch https://deps.files.ghostty.org/ghostty-themes-release-20260629-161812-8c97c3c.tgz
```
