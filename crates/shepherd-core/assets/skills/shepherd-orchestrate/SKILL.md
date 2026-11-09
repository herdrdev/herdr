---
name: shepherd-orchestrate
description: Use when you are asked to plan, delegate or run work with Shepherd (tasks, patterns, coder/reviewer agents), to check that Shepherd is enabled, or to unblock an agent that never received its task. Explains the CLI, the safe order of steps and how to recover.
---

# Orchestrating with Shepherd

Shepherd runs coding agents in terminal panes, each with a role, a vendor and (optionally) its own git worktree. The `shepherd` binary in `PATH` talks to the running server. Never guess syntax: the installed binary is the authority.

```bash
shepherd --help                  # terminal/pane/agent commands
shepherd task                    # prints the Shepherd usage line (tasks, patterns, spawn, topics)
shepherd config path             # where the config files are
shepherd config validate         # is the config valid?
```

## 1. Check the environment first

1. `shepherd status server` — is a server running? If not, ask the human to start Shepherd; do not start a second server.
2. `shepherd roles list` and `shepherd vendors list` — which roles exist and which vendors are installed.
3. If these commands say the Shepherd layer is disabled, enable it yourself:

   ```bash
   shepherd config enable
   ```

   It sets `[shepherd] enabled = true` in the server `config.toml`. The layer is read **only when the server starts**, so the human must restart the server (this closes the running terminals). Tell them; do not stop the server yourself.

## 2. Work through tasks

```bash
shepherd task new "Title" --description "What and why" --criteria "How we verify"
shepherd task list --json
shepherd task show <id> --json
shepherd task plan <id> --save          # brain: write the plan
```

One implementation agent (coder) per task. Reviewers and researchers stay read-only.

## 3. Start agents

```bash
shepherd spawn coder "<goal, files in scope, how to verify>" --task-id <id>
shepherd agents --json                  # id, role, vendor, state, pane
shepherd pattern list
shepherd pattern run <name> --task "Task #<id>: <objective>"
shepherd pattern runs                   # state of runs (pause/resume/stop <id>)
```

Start the pattern objective with `Task #<id>:` so the run is linked to the task.

## 4. Talk to an agent: pick the right channel

| You want | Use |
|---|---|
| Give a task or instruction to an agent now (also when it is idle or never got its prompt) | `shepherd agent prompt <id> "<text>" --wait` |
| Leave a note an active agent reads later | MCP `send_message` / the bus |
| See what the agent shows | `shepherd agent read <id> --lines 60` |
| Wait for it | `shepherd agent wait <id> --until idle --timeout 600000` |

`send_message` only writes to the bus. An idle agent does not look at the bus unless it calls `read_messages`, so a "resent" task through `send_message` may never be seen. Use `agent prompt` when it must be delivered.

## 5. When an agent does nothing

1. `shepherd agents --json` — look at `state` and `notice`.
2. `shepherd agent read <id> --lines 60` — look for a start-up dialog (trust this folder, login, model or quota warning). `blocked` means the agent is waiting for a human.
3. Do **not** type answers to trust, login or permission dialogs on your own; report exactly what the pane shows and let the human decide. Harmless navigation can be done with `shepherd agent send-keys <id> Enter` only if the human asked for it.
4. Once the pane is at the agent's input prompt, deliver the task with `shepherd agent prompt <id> "<text>" --wait`.
5. Do not start a second implementation agent for the same task while the first one is alive. If a vendor is unusable (quota, login), close that agent and spawn one with another vendor, after the human agrees.

## 6. Your workspace, tabs and panes

You run in a pane of a Shepherd workspace (`SHEPHERD_ENV=1`). Every agent Shepherd starts for you opens as a new **tab of the same workspace**, in the same folder (or in its own git worktree when its role asks for one), so the whole team stays together.

```bash
shepherd workspace list                     # spaces; yours is the focused one
shepherd tab list --workspace <id>          # one tab per agent
shepherd pane list --workspace <id>         # panes with their agent and status
shepherd agent read <id> --lines 60         # what an agent shows
shepherd tab create --workspace <id> --label notes   # an extra terminal next to the team
```

Prefer `shepherd spawn` / `shepherd pattern run` over creating tabs by hand: they register the agent, give it its skills and deliver its first prompt. Do not close workspaces or tabs you did not create, and do not move agents to another workspace.

## Rules

- Never `git push`, open PRs, merge or release without explicit human approval.
- Never read or print secrets from config files.
- Report what you checked and what you did not; do not claim a task was delivered or finished without seeing it in `agent read` or a result.
