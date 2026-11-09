# Shepherd in Rust: recommended architecture

As of 2026-11-03. Complements `docs/rust-migration.md` (porting order and criteria for retiring Go).

## What changes compared with the fork idea

Verified on this machine:

- There are **two shepherd products** installed, with different APIs:
  - `shepherd` 0.9.5: TUI and server with an NDJSON socket API (the one the Go code and `shepherd-core` use today).
  - `shepherd-app` + `shepherd-core` (`hc`): the **graphical version** (GPUI). Its terminals and agents run in `hc`; the app only shows them.
- `shepherd-app` **is extended without a fork**: the whole interface is files in `~/.config/shepherd-app` (layouts, keys) and JavaScript *mods* (gpui-kit) that can run `hc` and external programs (`shepherd.exec`, `shepherd.hc`, see `shepherd.d.ts`) and launch app commands (`tab.new {argv, task}`, `terminal.open`, ...).
- So **no fork is needed** to integrate Shepherd into the graphical app. A fork is only justified if something is needed that neither `hc` nor the mods expose. No such gap has been found so far.

## Recommended shape: core + port + two frontends

```
            ┌──────────────── shepherd-core (lib) ────────────────┐
            │ tasks · plans · workflow · roles · skills · MCP     │
            │ bus · registry · reconciliation · limits · gates    │
            └───────────────┬─────────────────────────────────────┘
                            │ trait Terminals  (the only point of contact with shepherd)
             ┌──────────────┴───────────────┐
        ShepherdSocket (shepherd 0.9.5)       HcCli (shepherd-core `hc` + `shepherd-app do`)
                            │
      ┌─────────────────────┴──────────────────────┐
  Frontend 1: CLI + terminal TUI              Frontend 2: shepherd-app mod
  (`shepherd`, ratatui later)                 (JS; runs `shepherd … --json`)
```

1. **`trait Terminals`**: open an agent in a terminal/tab (with worktree), send it a prompt or keys, read its screen, wait for it to stop, see its status, label it, close it, attach. `ShepherdSocket` is what already exists; `HcCli` runs `hc` (`run --bg`, `send`, `keys`, `read`, `wait`, `ls --json`, `label`, `kill`). The core never imports a concrete adapter, so the tests use a fake.
2. **Stable `--json` output** on all read commands (`agents`, `task list/show`, `roles list`, `runs`, `skills`). It is the contract consumed by the TUI and the mod. Documented in `docs/rust-json-schema.md`.
3. **Frontend 2 (shepherd-app mod)**: a panel that lists agents, tasks and runs with their actions (open the agent's terminal, kill, approve a gate), calling the `shepherd` binary with `--json`. No business logic in JavaScript. Validated with `shepherd-app check <folder>`.
4. **Frontend 1**: the existing CLI and, later, a ratatui TUI over the same contract (tree, sub-tabs and forms as in Go's M11 to M14).
5. **Daemon (`shepherd serve`)**: a long-running process with the registry, the bus, the MCP server and reconciliation against the active adapter. The CLI talks to it over a unix socket, like `shep`/`shepd`.

## Stages

| Stage | Content | Who |
|---|---|---|
| **R1** | `trait Terminals` + refactored `ShepherdSocket` + `HcCli` with fakes; `--json`; the 7 missing MCP tools; skills (catalog, global/role/session levels, per-vendor installation) | agent `agy` (`docs/rust-agent-brief.md`) |
| R2 | `shepherd serve`: persistent registry with counters that never go backwards, reconciliation, resume after restart, verification and resend of the first prompt, "needs you" notice, recovery of taken names | agent |
| R3 | `shepherd-app` mod (agents/tasks/runs panel) and git/branches/PR view | agent |
| R4 | Terminal TUI (ratatui) | agent |
| R5 | Import issues, `doctor`, `init`, editable configuration, installer; retire Go according to `rust-migration.md` | agent |

Each stage ends with `cargo test --workspace` green, without touching the safe behavior (push and PR always wait for a human).

## What the port must respect (learned from real tests with Go)

They are in `docs/vendors.md` and `docs/decisions.md`; the essentials:

- A prompt accepted by shepherd can be lost if the CLI is still starting: verify that the agent reacts and resend up to 3 times.
- A CLI stuck on a trust or login dialog never accepts its first prompt: mark it as "needs the human" and **never answer the dialog for the human**.
- Agent names are unique among open panes: a reused id collides; ids and the spawn counter **never go backwards** even if agents are cleaned.
- `done` means "finished its turn" and the pane stays alive; only `exited`, `killed` and `failed` are final.
- Editing a skill's text reaches the agent without restarting (claude and codex); adding or removing a skill requires restarting it.
- Push and PR always wait for explicit human approval, except for that run's `--auto-push`.
- Nothing Shepherd writes enters the user's repo (skills and instructions are injected per session).

## Open questions for the project owner

1. Is there already a herdr fork with its own changes? There is none on this machine. If there is, what capability does it provide that `hc` lacks?
2. Is the graphical app the main adoption target, or the terminal TUI? It changes the order of R3 and R4.
3. Is the `shepherd` 0.9.5 socket protocol kept, or is only `hc` left?

---

## Mapping `trait Terminals` → `ShepherdSocket` and `HcCli`

The module `crates/shepherd-core/src/terminals.rs` defines `trait Terminals` and selects the active adapter through the `SHEPHERD_TERMINALS` environment variable (`socket` | `hc`) or the `shepherd.backend` configuration key (`socket` by default):

| Method in `trait Terminals` | `ShepherdSocket` (`shepherd` 0.9.5 NDJSON socket) | `HcCli` (`shepherd-core` `hc` CLI) |
|---|---|---|
| `open_agent(&OpenAgentRequest)` | `workspace.list` / `workspace.create` or `worktree.create` → `tab.create` (with `env`) → `pane.rename` → `metadata.report` → `agent.start` (or `pane.send_text` if `shepherd_agent` is empty) | Git worktree (`git worktree add -B <branch> <path> <base>`) if applicable → `hc run --bg --json --outlive-parent --no-notify --name <agent_id> --cwd <cwd> --label role=<role> --label agent=<agent_id> --label vendor=<vendor> --label label=<label> -- <command> <args...>` (with `env` variables in the child process) |
| `prompt(target, text)` | `agent.prompt { agent, text }` (`agent_blocked` → `PromptOutcome::Blocked`) | `hc send --json <target> <text>` (exit code `9` or `error.kind == "blocked"` → `PromptOutcome::Blocked`) |
| `send_text(target, text)` | `pane.send_text { pane_id, text, enter: true }` | `hc send --json <target> <text>` |
| `send_keys(target, keys)` | `pane.send_keys { pane_id, keys }` | `hc keys --json <target> <keys...>` |
| `read_screen(target, lines)` | `pane.read { pane_id, lines }` | `hc read --json --lines <lines> <target>` (extracts `.text` from `ReadResult`) |
| `wait(target, timeout)` | `agent.wait { agent, timeout_ms }` | `hc wait --json --timeout <secs> <target>` |
| `agent_status(target)` | `agent.list {}` (filters by `name` or `pane_id`) | `hc ls --json` (filters `.terminals` by `name`, `address` or `labels.agent`) |
| `label(target, labels)` | `pane.rename` (if it includes `label`) + `metadata.report { pane_id, source: "shepherd", status, data }` | `hc label --json <target> <k=v>...` |
| `close(target)` | `tab.close { tab_id }` | `hc kill --json <target>` |
| `attach(target, takeover)` | `shepherd [--session <s>] agent attach <target> [--takeover]` or `shepherd session attach <s>` | `hc attach [target]` |

---

## R1 status (completed)

### 1. Modules in `crates/shepherd-core` after R1

| File | Status in R1 |
|---|---|
| `src/terminals.rs` | **New in R1**: `trait Terminals`, `ShepherdSocket`, `HcCli`, `InMemoryTerminals`, backend selection by `SHEPHERD_TERMINALS` and `shepherd.backend` (`socket\|hc`, default `socket`). |
| `src/shepherd.rs` | Extended with `send_keys`, `read_pane` and `wait_agent` over the NDJSON socket client, wrapped by `ShepherdSocket`. |
| `src/spawn.rs` | Refactored to depend exclusively on `&dyn Terminals` (`spawn_with_terminals`, `spawn_with_options`, `spawn_child`), checking `can_spawn`, `max_spawn_depth`, `max_concurrent_agents`, `use_worktree`, and installing session skills through `skills::install`. |
| `src/workflow.rs` | Refactored to run `run_coders_with_terminals`, `review_and_check`, `revise_coder` and `revise_agent` through `&dyn Terminals`. |
| `src/skills.rs` | **New in R1**: catalog with priority `Repo > User > Default (embedded from skills/)`, `_global/`, `effective` / `effective_for` with `lang-*` auto-activation and `SkillAdjust` (`Global`, `Role`, `Session`), `edited` detection, frontmatter validation (`validate`), management (`name_ok`, `list_in`, `new_skill`, `override_skill`, `toggle` with automatic rollback if another layer shadows it), and per-session installation (`claude-plugin`, `codex-developer`, `worktree-file`, `prompt`). |
| `src/mcp.rs` | Extended from 6 to **13 MCP tools**: added `spawn_agent`, `wait_for`, `get_result`, `list_agents`, `read_output`, `ask_docs` and `get_workspace_instructions` next to `submit_result`, `get_task_context`, `read_messages`, `send_message`, `get_diff` and `update_task_context`, respecting the permissions of `internal/daemon/mcp.go`. |
| `src/lib.rs` and `src/main.rs` | Added `--json` support in `shepherd agents --json`, `shepherd task list [--archived] --json`, `shepherd task show <id> [section] --json` and `shepherd roles list --json`, with guaranteed omission of tokens (`Agent::public()`). Schemas documented in `docs/rust-json-schema.md`. |

### 2. Verified vs. not verified live

- **Verified with `cargo test --workspace` (52 tests green, 0 warnings)**:
  - Adapter selection (`SHEPHERD_TERMINALS` > `shepherd.backend` > `"socket"`), configuration validation.
  - `InMemoryTerminals` in unit tests of `terminals`, `spawn`, `workflow` and `mcp`.
  - `HcCli` against a fake `hc` script in a temporary directory that checks the exact arguments and JSON parsing of `run`, `send` (including the block with code 9), `keys`, `read`, `wait`, `ls`, `label`, `kill` and `attach`.
  - `ShepherdSocket` / `ShepherdClient` against a temporary `UnixListener` checking the NDJSON protocol.
  - No divergence between the 13 skills embedded in Rust (`include_str!`) and the files in `skills/` in the repository tree.
  - Priority `Repo > User > Default`, levels `Global / Role / Session`, `edited`, `toggle` with rollback on shadowing, and session installation in the 4 styles (`claude-plugin`, `codex-developer`, `worktree-file`, `prompt`).
  - `--json` schemas and total absence of the `token` field in `agents --json` and `list_agents`.
  - The 13 MCP tools, `can_spawn` permissions, `max_spawn_depth` and `max_concurrent_agents` limits, restriction of `update_task_context` to `brain`/`docs`, `read_output`, `ask_docs` and `get_workspace_instructions`.
- **Not verified against real live processes**:
  - Because of the session's safety rules, no destructive commands or real spawns were run against the host machine's `hc` daemon or `shepherd-app` (`hc run`, `hc kill`, `shepherd-app do`).

### 3. What remains for R2+

- **R2 (`shepherd serve`)**: persistent daemon with a reconciliation loop against `Terminals`, verification and resend (up to 3 times) of the first prompt if the CLI is slow to start, detection of the `"needs you"` notice when an agent gets blocked, and recovery from taken names.
- **R3**: `shepherd-app` mod consuming `shepherd ... --json`.
- **R4**: terminal TUI in `ratatui`.
- **R5**: GitHub issue import, `doctor`, `init`, installer and retirement of the Go tree according to `docs/rust-migration.md`.
