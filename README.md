# shepherd

<p align="center">
  <a href="#install">install</a> · <a href="#quick-start">quick start</a> · <a href="#built-on-herdr">built on herdr</a> · <a href="#license">license</a>
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-666666?labelColor=333333" alt="Apache 2.0 license" /></a>
  <a href="https://github.com/herdrdev/herdr"><img src="https://img.shields.io/badge/based%20on-herdr%200.9.5-666666?labelColor=333333" alt="based on herdr 0.9.5" /></a>
  <img src="https://img.shields.io/badge/status-experimental-666666?labelColor=333333" alt="experimental" />
</p>

---

**the orchestrator for your coding agents.**

shepherd is a fork of [herdr](https://github.com/herdrdev/herdr) that adds multi-agent orchestration on top of herdr's terminal runtime. you keep the native terminal of every agent; shepherd decides who works on what, in which worktree, and how they talk to each other.

- **agents with roles** — claude, codex and gemini (agy) as `brain`, `coder`, `reviewer`, `docs` or `tester`. routing by role, with fallbacks and a different-vendor rule for reviews. `shepherd spawn coder "…"`.
- **one git worktree per agent** — each coder gets its own branch and worktree, so agents never step on each other. you integrate with plain git; shepherd never merges or pushes on its own.
- **multi-agent patterns** — orchestrator-worker, hierarchical, blackboard and market as YAML teams you can copy, edit, validate and run. `shepherd pattern run webteam --task "…"`.
- **event bus** — topics, partitions, consumer groups, offsets and replay, exposed to agents as MCP tools and to you as `shepherd topic …`. publishing wakes idle agents.
- **tasks and gated runs** — tasks with specs and acceptance criteria; plan, implement, review, check and publish with explicit human approval.
- **local snapshots** — automatic safety copies of every agent's worktree in hidden git refs, with diff and safe restore. no network, no extra dependencies.
- **a shepherd tab in the terminal** — `ctrl+b a` opens agents, tasks, runs, patterns, skills, git and events as an in-window section with herdr's look, keyboard and mouse. no pop-ups.
- **cli for everything** — every action is a command with `--json` output.
- **one rust binary, no electron** — runs in whatever terminal you already use.

---

## install

shepherd is experimental and has no binary releases yet; build it from source.

```bash
git clone https://github.com/Rivaldito/shepherd.git
cd shepherd
./cargo-fork.sh build --release     # or: cargo build --release
```

requirements: Rust 1.96.1+ and Zig 0.16.0 (to build the vendored terminal library). the binary is `target/release/shepherd`.

## quick start

enable the orchestrator in `~/.config/shepherd/config.toml`:

```toml
[shepherd]
enabled = true
```

start it where the work lives, then open the shepherd section:

```bash
shepherd                 # then press ctrl+b a, or click the "Shepherd" tab
shepherd config check    # validate your configuration
shepherd spawn coder "Add GET /health and a test. Commit." --vendor claude --model haiku
shepherd agents
```

`ctrl+b q` detaches, `shepherd` reattaches.

## docs

design notes and project status: [SHEPHERD.md](SHEPHERD.md). user guides live on the `feat/native-ui` branch.

## built on herdr

shepherd would not exist without [herdr](https://github.com/herdrdev/herdr) ([herdr.dev](https://herdr.dev)), created by the herdr contributors and licensed under the Apache License 2.0. the terminal multiplexer, pty management, session persistence, agent detection, client/server protocol, socket api and plugin system in this repository are their work. shepherd is based on herdr 0.9.5, keeps its history, and merges upstream changes regularly. thank you to everyone who builds herdr 🐑

what differs from herdr:

- **focus** — herdr manages agent panes; shepherd adds coordination: roles, worktrees, patterns, bus, tasks and snapshots.
- **separate config and state** — `~/.config/shepherd`, with a read-only fallback to herdr's `config.toml`.
- **no upstream auto-update** — update checks against herdr's release channels are disabled so they never replace the shepherd binary.
- **human gates** — changes and git publishing need explicit confirmation unless you override it.

shepherd is an independent fork; it is not affiliated with or endorsed by the herdr project. see [NOTICE](NOTICE) for the full attribution and the list of modified upstream files.

## development

```bash
./cargo-fork.sh build                 # debug build
./cargo-fork.sh test -p shepherd-core # orchestrator core tests
./cargo-fork.sh test --bin shepherd -- shepherd:: client::shell protocol:: api:: config::

scripts/rename-from-herdr.sh --check              # no leftover upstream names
scripts/upstream-sync.sh --dry-run --allow-branch shepherd   # preview merging upstream herdr
scripts/shepherd-iso.sh start                     # isolated session for manual checks
```

code style follows herdr: rustfmt and clippy clean on shepherd-owned files, a typed socket api, and minimal, additive hooks in herdr files. see [AGENTS.md](AGENTS.md) and [CONTRIBUTING.md](CONTRIBUTING.md).

## license

shepherd is licensed under the [Apache License 2.0](LICENSE). copyright (c) 2026 Sebastián Araque Rivaldito for the shepherd additions; the herdr code remains copyright of the herdr contributors under the same license. see [NOTICE](NOTICE).
