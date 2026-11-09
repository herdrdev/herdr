# Getting started

This guide takes you from nothing to a running team of agents. It assumes Linux or macOS and a git repository you want the agents to work on.

## 1. Requirements

| Tool | Why | Check |
|---|---|---|
| Rust 1.96.1 or newer | builds Shepherd | `rustc --version` |
| Zig 0.16.0 | builds the vendored terminal library | `zig version` |
| git | worktrees, snapshots | `git --version` |
| At least one agent CLI: `claude`, `codex` or `agy` (gemini) | the agents themselves | `shepherd vendors list` after building |
| `gh` (optional) | importing issues and opening PRs | `gh --version` |

If Zig is not on your `PATH`, point the build at it: `export ZIG=/path/to/zig`.

## 2. Install

On Linux and macOS (x86_64 and arm64) the quickest way is the installer, which downloads the binary of the latest release, checks its SHA-256 and puts it in `~/.local/bin`:

```bash
curl -fsSL https://raw.githubusercontent.com/Rivaldito/shepherd/main/install.sh | sh
```

Set `SHEPHERD_INSTALL_DIR` to install somewhere else, or `SHEPHERD_VERSION=v0.1.0-beta` to pick a release. You can also download a binary from the [releases page](https://github.com/Rivaldito/shepherd/releases). If you installed the binary, skip the build below.

## 2b. Build from source

```bash
git clone https://github.com/Rivaldito/shepherd.git
cd shepherd
./cargo-fork.sh build --release        # or: cargo build --release
```

The binary is `target/release/shepherd`. Put it on your `PATH`, or run it from there. For a quick local try you can use the debug build (`./cargo-fork.sh build`, binary in `target/debug/shepherd`). A debug build reads its configuration from `~/.config/shepherd-dev/` instead of `~/.config/shepherd/`.

## 3. Enable the orchestrator

Without this switch Shepherd behaves like a plain terminal multiplexer. Create `~/.config/shepherd/config.toml`:

```toml
[shepherd]
enabled = true
```

Check it:

```
$ shepherd config check
config: ok
```

## 4. Start in your project

Go to a git repository with at least one commit and start Shepherd there. The project's `.shepherd/` folder (tasks, patterns, configuration) is created inside it.

```bash
cd ~/my-project
shepherd                 # starts the server and attaches the terminal UI
```

Inside the UI, `Ctrl+b a` opens the Shepherd section. `Ctrl+b q` detaches and leaves everything running; run `shepherd` again to reattach.

Everything below can also be done from another terminal with the CLI, which talks to the same server.

## 5. See what you have

```
$ shepherd roles list
brain      claude/opus
coder      codex/default
docs       gemini/default
reviewer   gemini/default
tester     claude/sonnet

$ shepherd vendors list
claude   claude   available
codex    codex    available
gemini   agy      available
```

`available` means the command was found on your `PATH`. If a vendor shows as unavailable, roles that use it fall back to their alternatives, or you can change the role (see [agents-roles-skills.md](agents-roles-skills.md)).

## 6. Run one agent

```bash
shepherd spawn coder "Add GET /health to main.go and a test. Commit." --vendor claude --model haiku
shepherd agents
```

```
ID       STATE    ROLE   VENDOR  MODEL  TASK
coder-1  working  coder  claude  haiku  Add GET /health to main.go and a test. Commit.
```

The `coder` role has `worktree: true`, so the agent works in its own worktree on a branch named `shep/<n>-...` under `~/.shepherd/worktrees/<repo>/`. The first time an agent starts it may ask you to trust the folder. Open its terminal and answer yourself:

```bash
shepherd attach coder-1
```

When it is done, integrate its work with plain git:

```bash
git merge --no-edit shep/1-<slug-of-the-task>
```

Shepherd never merges for you.

## 7. Run a team

Copy a template, check it and run it with an objective:

```
$ shepherd pattern list
NAME                 KIND                 SOURCE   ...
blackboard           blackboard           builtin
hierarchical         hierarchical         builtin
market               market               builtin
orchestrator-worker  orchestrator-worker  builtin

$ shepherd pattern new webteam --from orchestrator-worker
created pattern 'webteam' at .shepherd/patterns/webteam.yaml
$ shepherd pattern validate webteam
pattern is valid
```

Open `.shepherd/patterns/webteam.yaml` and set the vendor and model you want for each agent (the templates use each role's default). Then run it:

```bash
shepherd pattern run webteam --task "One worker adds GET /version with a test; another adds a logging middleware with a test. Each commits in its own worktree."
shepherd pattern runs          # state of the run
shepherd agents                # the agents it started
shepherd topic groups orders   # how the work is split among workers
shepherd topic tail results    # what the workers report
```

When the run reaches `done`, merge each worker's branch with git. Use `shepherd pattern pause <id>`, `resume <id>` or `stop <id> --confirm` to control a run. The full reference is in [patterns.md](patterns.md).

## 8. Create a task

Tasks hold a title, a specification and acceptance criteria. They live in `.shepherd/tasks/` and can be linked to agents and runs.

```
$ shepherd task new "Add login" --description "A login form" --criteria "tests pass"
created task 1-add-login
$ shepherd task list
ID  STATE  TITLE      ISSUE  PR
1   new    Add login  -      -
```

Start a run for a task from the Tasks view, or begin a pattern objective with `Task #1:` so the run is linked to it.

## 9. Try it without touching your setup

To explore safely, use an isolated session. It runs with its own temporary home, configuration and state under `/tmp/shfiso`, so your real setup is not touched. Agents you start inside it are still real, so spawn only what you mean to run:

```bash
BIN=target/debug/shepherd scripts/shepherd-iso.sh start   # isolated session
scripts/shepherd-iso.sh screen     # what the UI shows
scripts/shepherd-iso.sh stop
```

For the native app, `native/dev/demo.sh` (see [native-app.md](native-app.md)) opens the window against a throwaway server with sample data and a stand-in agent, so no real agent ever runs.

## 10. Where things live

| What | Where |
|---|---|
| App configuration (`[shepherd]` section, keys, theme) | `~/.config/shepherd/config.toml`, and `<repo>/.shepherd/config.toml` |
| Roles, vendors, skills, workflow | `~/.config/shepherd/config.yaml`, and `<repo>/.shepherd/config.yaml` |
| Tasks | `<repo>/.shepherd/tasks/` |
| Patterns | `<repo>/.shepherd/patterns/` (project) or `~/.config/shepherd/patterns/` (user) |
| Project skills | `<repo>/.shepherd/skills/<name>/SKILL.md` |
| Agent state, event bus, registry | `~/.local/state/shepherd` |
| Agent worktrees | `~/.shepherd/worktrees/<repo>/` |
| Snapshots | hidden git refs `refs/shepherd/snapshots/...` in your repo |

`shepherd config path` prints the exact locations on your machine. `shepherd config validate` checks your roles and vendors file.

## 11. Limits worth knowing

Add these to `config.toml` if the defaults do not suit you:

```toml
[shepherd.limits]
max_concurrent = 4        # agents working at once (default 4)
max_depth = 2             # how deep agents may start agents (default 2)
run_quiescence_secs = 30  # idle time before a run counts as done (default 30)

[shepherd.snapshots]
max_per_agent = 50
interval_secs = 300       # periodic snapshot while an agent works (0 disables)
```

If a pattern run would exceed `max_concurrent`, `shepherd pattern run` asks for `--confirm-over-limit`.

## Known rough edges

- A new agent may ask for trust or login in its terminal; a person has to answer.
- Agents inherit your global vendor configuration (for example `~/.claude/CLAUDE.md`). If it requires plan approval before editing, an agent can wait for you. Put the approval in the task.
- An agent waiting for a human answer counts as idle, so a run can briefly show `done` and reopen when work continues.

## Next steps

- [agents-roles-skills.md](agents-roles-skills.md): tune roles, add skills.
- [patterns.md](patterns.md): design your own team.
- [topics-and-events.md](topics-and-events.md): understand and watch the bus.
- [native-app.md](native-app.md): the desktop app.
