<!-- Modified from herdr by the vimeflow project — see FORK.md -->

# vimeflow-terminal

**an opinionated downstream fork of [herdr](https://github.com/herdrdev/herdr).**

<p align="center">
  <a href="https://github.com/herdrdev/herdr">upstream herdr</a> ·
  <a href="https://herdr.dev/docs/">herdr docs</a> ·
  <a href="FORK.md">what this fork changes</a> ·
  <a href="#try-it">try it</a>
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-666666?labelColor=333333" alt="Apache 2.0 license" /></a>
</p>

---

## try herdr first

**If you have not used [herdr](https://herdr.dev), start there.** Herdr is the
real thing: an agent multiplexer that lives in your terminal — every agent's
state at a glance, detach and reattach from anywhere, a pure socket API agents
can drive themselves, keyboard and mouse both first-class, one Rust binary.

```bash
curl -fsSL https://herdr.dev/install.sh | sh
```

Herdr is installable, documented, released on a schedule, and supported. This
fork is none of those things yet. Almost everything you would want, herdr
already does — and it does it better, because upstream ships it.

Come back here only if you have used herdr and specifically want the
opinionated layer described below.

## what this fork is

Vimeflow was an Electron desktop app for coding agents. In August 2026 it
pivoted to terminal-native, rebuilt as a **tracking fork of herdr v0.8.0**.

The split of labor is deliberate:

- **herdr supplies the engine** — PTY ownership, VT emulation via vendored
  libghostty-vt, the workspace → tab → pane model, git and worktree state,
  agent detection, notifications, the plugin system, the socket API.
- **vimeflow supplies an opinionated layer on top** — agent observability as
  built-in chrome rather than an optional plugin, and a set of workflow
  features that assume you are running several coding agents at once.

Shortest description: **the opinionated herdr**. Upstream stays general; this
fork makes choices for you. If a choice here turns out to be right for
everybody, it belongs upstream, not in a fork.

This is a *tracking* fork, not a hard fork. Upstream releases are merged in on
a per-release cadence, every edit to an upstream file is logged, and the goal
is to stay mergeable — not to drift.

## what's here today

Everything herdr v0.8.0 does, plus:

- **built-in agent watcher** — coding-agent observability (transcript
  watchers, lifecycle, metrics, notifications) is compiled into the binary and
  runs with the server. No plugin to install, link, or keep in sync. CLI under
  `herdr watcher`.
- **automatic pane titles** — pane labels follow the agent's session title as
  it works. A manual rename always wins and is never overwritten.
- **Agents sidebar cards** — the sidebar's Agents section renders agent cards
  (lifecycle plus context, cache, cost, model, tools, traces) instead of a
  token-row list. The focused agent's card auto-expands. Configurable live:

  ```toml
  [ui.sidebar]
  agents_view = "cards"    # or "legacy" for herdr's row list
  agents_hide_idle = false
  ```

- **no phone-home** — self-update, hosted manifest fetches, and product
  announcements are deliberately disabled. This fork will never install stock
  herdr over itself.

The native features above are **Unix-only** (macOS and Linux). Windows builds
the upstream feature set.

## what's coming

No dates. Roughly in order:

- **card cursor model** — section focus and `j`/`k`/`o`/`z` navigation for the
  Agents cards, replacing today's expand-follows-focus behavior.
- **notification island** — a compact top-center overlay aggregating agent
  working state (herdr's OSC 9;4 progress unioned with watcher lifecycle) and
  the notification queue.
- **pane cards and the worktree flow** — card-style pane headers with agent
  glyph, state, and worktree badge; and *"new agent pane in a fresh worktree"*
  collapsed into a single action that splits the current tab instead of
  spawning a new workspace.
- **agent rows in the navigator** — per-agent state chips and latest task
  message under each workspace/tab entry; click to focus the pane.
- **local hunk view** — a TUI diff pane over an in-process git engine:
  worktree-scoped file list, hunk-level rendering and navigation, read-only
  first.

Deliberately **not** done yet: the branding rename. The binary, config paths,
socket names, and `HERDR_*` environment variables are all still `herdr`, so
existing plugins and operator workflows keep working. See the deferred
branding surface in [`FORK.md`](FORK.md).

## try it

There are no prebuilt binaries, no installer, and no release channel. Build
from source:

```bash
git clone https://github.com/winoooops/vimeflow-terminal
cd vimeflow-terminal
cargo build --release
./target/release/herdr
```

Requires Rust 1.96.1 (pinned in `rust-toolchain.toml`) and **Zig 0.15.2** to
compile the vendored libghostty-vt — a newer Zig on `PATH` will fail. On a cold
Zig cache, run `scripts/preseed_zig_cache.sh` first.

The binary is still named `herdr` and reads `~/.config/herdr`, so it will share
state with a stock herdr install. Point `HERDR_SOCKET_PATH` elsewhere if you
want the two to coexist.

```bash
just test     # unit tests + maintenance checks
just check    # the full gate: lint, tests, Windows-target clippy
```

## how this fork tracks herdr

- `main` is the product branch and carries all Vimeflow work.
- `master` is a fast-forward-only mirror of `upstream/master`. Nothing is
  committed there.
- Upstream releases are merged into `main` through review branches, once per
  release rather than once per commit.
- Every modified upstream file carries an in-file change notice and a row in
  the [`FORK.md`](FORK.md) registry; non-commentable files are listed in
  [`MODIFICATIONS`](MODIFICATIONS).

[`FORK.md`](FORK.md) is the full record: fork base commit, the path-by-path
upstream-edit registry, the merge procedure, and the known-baseline test
behavior. [`AGENTS.md`](AGENTS.md) is the guidance for AI agents working in
this repository.

## license and attribution

This fork is licensed under the [Apache License 2.0](LICENSE), the same license
as herdr, and preserves upstream's LICENSE unchanged.

Herdr is copyright the herdr contributors and is created and maintained by
[@ogulcancelik](https://github.com/ogulcancelik). This project exists only
because that work is open source, and upstream deserves the credit and the
support — **if this fork is useful to you, [sponsor
herdr](https://github.com/sponsors/ogulcancelik)**, not this fork.

Vendored dependencies carry their own licenses: `libghostty-vt` (MIT, ©
Mitchell Hashimoto) and `portable-pty` (MIT, © Wez Furlong). The vendored
`libghostty-vt/pkg` tree includes additional third-party material under other
terms; any future binary distribution of this fork must ship a license
inventory covering it.

"herdr" is the upstream project's name, used here only to describe this fork's
origin. This project claims no rights to it.
