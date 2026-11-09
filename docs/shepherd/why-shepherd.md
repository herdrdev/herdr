# Why Shepherd

Shepherd is a fork of [herdr](https://github.com/herdrdev/herdr), a terminal multiplexer built for coding agents. herdr gives every agent a real terminal and tells you what each one is doing. Shepherd keeps all of that and adds the layer above it: **who works on what, in which worktree, and how the agents talk to each other.**

This page explains what you gain. The other guides show how to use each piece:

| Guide | What it covers |
|---|---|
| [getting-started.md](getting-started.md) | Build, enable and run your first agents |
| [agents-roles-skills.md](agents-roles-skills.md) | Agent lifecycle, roles, vendors, skills and the MCP tools |
| [patterns.md](patterns.md) | Multi-agent teams as YAML: the four topologies, validation and runs |
| [topics-and-events.md](topics-and-events.md) | The event bus: topics, partitions, consumer groups, replay |
| [native-app.md](native-app.md) | The native desktop app and how it relates to the terminal |

## The problem

Running several coding agents at once works for a while. Then it gets messy:

- Two agents edit the same files and overwrite each other.
- You forget which agent has which task, or which one is stuck waiting for you.
- Agents cannot hand work to one another, so you copy text between terminals.
- A bad edit destroys an hour of work and there is nothing to go back to.
- Every team layout (planner and workers, reviewer after coder, ...) is rebuilt by hand each time.

## What Shepherd adds

### Agents with roles, not just panes

Each agent has a **role** (`brain`, `coder`, `reviewer`, `docs`, `tester`, or your own). A role decides the default vendor and model, whether the agent gets its own worktree, whether it may start other agents, and which skills it receives. You can run claude, codex and gemini side by side and route work by role, with fallbacks when a vendor is not installed. A reviewer can be forced to use a different vendor than the coder, so a model never grades its own work.

```bash
shepherd spawn coder "Add GET /health and a test. Commit." --vendor claude --model haiku
```

### One git worktree per agent

A role with `worktree: true` gets its own `git worktree` and a branch named `shep/<n>-<task>`. Agents cannot step on each other, and every result is a normal branch you can diff, test and merge. Shepherd never merges or pushes for you.

### Teams as files

A **pattern** is a YAML file describing a team: which agents, with which roles, vendors and models, which topics they use to talk, and who starts. Four topologies ship as templates: `orchestrator-worker`, `hierarchical`, `blackboard` and `market`. Copy one, edit it, validate it and run it:

```bash
shepherd pattern new webteam --from orchestrator-worker
shepherd pattern validate webteam
shepherd pattern run webteam --task "Add GET /version and a logging middleware"
```

Patterns live in your repo (`.shepherd/patterns/`), so the team layout is versioned with the code.

### An event bus agents can use

Agents coordinate through **topics** with partitions, consumer groups, offsets and replay, the model used by event streaming systems. Publishing to a topic wakes idle agents that subscribe to it. Agents see the bus as MCP tools; you see it as `shepherd topic ...` and as the Events view. See [topics-and-events.md](topics-and-events.md).

### Skills that follow the role

A **skill** is a `SKILL.md` with instructions (code review, conventional commits, language conventions, ...). Shepherd installs the skills a role needs into each agent, in the way each vendor expects. You can write your own at project or user level and assign them to roles or to every agent. See [agents-roles-skills.md](agents-roles-skills.md).

### Human gates

Shepherd stops where a person should decide. A run can wait for you to approve its plan. Pushing a branch and opening a PR always wait for explicit approval unless you pass `--auto-push`. Agents stuck on a trust or login dialog are marked as needing you, and Shepherd never answers those dialogs for you.

### Safety snapshots

Shepherd takes automatic copies of every agent's worktree (when it goes idle, when it finishes, and periodically while it works) and stores them in hidden git refs. You can diff against any snapshot and restore all or some files. Before any restore a `pre-restore` snapshot is taken, so a restore can be undone. No network and no extra dependencies.

### Two ways in, one set of data

- **The terminal.** A Shepherd section inside the multiplexer (`Ctrl+b a`), with seven views driven by keyboard or mouse.
- **The native app.** A desktop window built with iced that shows the same data with graphs, an event feed and editors. A switch (`Ctrl+Shift+S`) moves between the terminal and the Shepherd panel.
- **The CLI.** Every action is a command, and most commands accept `--json`.

All three talk to the same server, so what you create in one appears in the others.

## How it compares

| | Plain terminal multiplexer | Shepherd |
|---|---|---|
| Agent terminals, detection, sessions | yes | yes (herdr's) |
| Role-based routing and vendor fallbacks | no | yes |
| Isolated worktree per agent | by hand | automatic per role |
| Team layouts | by hand | YAML patterns with validation |
| Agent-to-agent messaging | copy and paste | event bus with partitions and groups |
| Plan approval and publish gates | no | yes |
| Rollback of an agent's edits | no | snapshots with safe restore |
| Skills per role | no | yes |

## What Shepherd deliberately does not do

- It does not merge, push or open pull requests on its own.
- It does not answer permission or login prompts for you.
- It does not replace your agents: claude, codex and gemini keep running in their own terminals with their own interfaces.
- It does not send your code anywhere; everything runs locally.

## Staying close to herdr

Shepherd is a fork designed to be easy to keep in sync. Upstream changes are merged with a repeatable routine ([../UPSTREAM-SYNC.md](../UPSTREAM-SYNC.md)), the modified upstream files are listed in [NOTICE](../../NOTICE), and Shepherd's own code lives in separate modules with small, additive hooks in herdr files.
