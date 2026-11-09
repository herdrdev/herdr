# shepherd

> **a fork of [herdr](https://github.com/herdrdev/herdr)** (apache-2.0), forked from herdr 0.9.5 at upstream commit `ee5bfb5`. the herdr history is preserved below the shepherd commit; shepherd is not affiliated with the herdr project.

<p align="center">
  <a href="#install">install</a> · <a href="#quick-start">quick start</a> · <a href="#try-it-from-scratch">try it from scratch</a> · <a href="#documentation">docs</a> · <a href="#built-on-herdr">built on herdr</a> · <a href="#license">license</a>
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-666666?labelColor=333333" alt="Apache 2.0 license" /></a>
  <a href="https://github.com/herdrdev/herdr"><img src="https://img.shields.io/badge/fork%20of-herdr%200.9.5-666666?labelColor=333333" alt="fork of herdr 0.9.5" /></a>
  <a href="https://github.com/Rivaldito/shepherd/releases"><img src="https://img.shields.io/badge/release-0.1.0--beta-666666?labelColor=333333" alt="release 0.1.0-beta" /></a>
  <img src="https://img.shields.io/badge/status-beta-666666?labelColor=333333" alt="beta" />
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

## a look

the native app (`ctrl+shift+s` switches between the terminal and the shepherd panel), shown with a small sample project:

<p align="center">
  <img src="docs/shepherd/media/overview.png" alt="overview: runs, agents, tasks and the herd pulse" width="900" />
</p>

<p align="center">
  <img src="docs/shepherd/media/patterns.png" alt="pattern graph editor with the api-team orchestrator-worker pattern" width="440" />
  <img src="docs/shepherd/media/agents.png" alt="agents view with role, vendor, model and state" width="440" />
</p>

more pictures of every view are in [docs/shepherd/native-app.md](docs/shepherd/native-app.md#screenshots).

## install

shepherd is in beta (0.1.0-beta) and ships no binaries yet; build it from source.

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

## try it from scratch

a complete walk from an empty machine to a team of agents. it takes about ten minutes plus the first build.

**1. get the tools.** you need git, Rust 1.96.1+, Zig 0.16.0 and at least one agent cli (`claude`, `codex` or `agy`). check with `rustc --version`, `zig version` and `git --version`. if zig is not on your `PATH`, run `export ZIG=/path/to/zig`.

**2. build it.**

```bash
git clone https://github.com/Rivaldito/shepherd.git
cd shepherd
./cargo-fork.sh build --release
export PATH="$PWD/target/release:$PATH"      # so you can just type `shepherd`
```

**3. turn the orchestrator on.**

```bash
mkdir -p ~/.config/shepherd
printf '[shepherd]\nenabled = true\n' > ~/.config/shepherd/config.toml
shepherd config check                        # prints: config: ok
```

**4. make a repository to play in.** shepherd works on git repositories with at least one commit.

```bash
mkdir ~/shepherd-playground && cd ~/shepherd-playground
git init -q && git commit -q --allow-empty -m "init"
```

**5. look at what you have.**

```bash
shepherd roles list        # brain, coder, docs, reviewer, tester and their default vendors
shepherd vendors list      # which agent clis were found on your PATH
shepherd pattern list      # the four built-in team templates
```

**6. start shepherd and run one agent.** in one terminal run `shepherd`, then press `ctrl+b a` to open the shepherd section. in a second terminal, in the same folder:

```bash
shepherd spawn coder "Create hello.txt containing 'hi' and commit it." --vendor claude --model haiku
shepherd agents            # watch it go from starting to working to idle
shepherd attach coder-1    # open its terminal; answer a trust prompt if it asks
```

the agent works in its own git worktree on a branch named `shep/<n>-...`. when it finishes, merge it yourself with `git merge`; shepherd never does.

**7. run a team.**

```bash
shepherd pattern new webteam --from orchestrator-worker
shepherd pattern validate webteam
shepherd pattern run webteam --task "Create a.txt containing A and b.txt containing B, one worker each. Commit."
shepherd pattern runs      # state of the run
shepherd topic tail results
```

**8. optional: the native app.**

```bash
(cd native && cargo build --release -p shepherd-native)
native/dev/demo.sh         # opens the window against a throwaway server with sample data
```

`ctrl+shift+s` switches between the terminal and the shepherd panel. to connect it to your own server instead, see [docs/shepherd/native-app.md](docs/shepherd/native-app.md).

to explore without touching your own configuration at all, use `scripts/shepherd-iso.sh start` for an isolated session. the details of every step are in [docs/shepherd/getting-started.md](docs/shepherd/getting-started.md).

## documentation

| guide | what it covers |
|---|---|
| [why shepherd](docs/shepherd/why-shepherd.md) | what shepherd adds on top of herdr and when it helps |
| [getting started](docs/shepherd/getting-started.md) | build, enable, first agent, first team, where files live, limits |
| [agents, roles and skills](docs/shepherd/agents-roles-skills.md) | agent lifecycle, roles, vendors, writing and assigning skills, mcp tools, snapshots |
| [patterns](docs/shepherd/patterns.md) | the four team topologies, the yaml format, validation, run states, a full example |
| [topics and events](docs/shepherd/topics-and-events.md) | the event bus: topics, partitions, consumer groups, replay |
| [native app](docs/shepherd/native-app.md) | the desktop window, its ten views and how to connect it |

## keeping up with herdr

this fork follows herdr with a repeatable routine, described in [docs/UPSTREAM-SYNC.md](docs/UPSTREAM-SYNC.md).

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
