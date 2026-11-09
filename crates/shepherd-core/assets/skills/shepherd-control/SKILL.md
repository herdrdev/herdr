---
name: shepherd-control
description: "Use when you are an agent running inside Shepherd and need to inspect or control its terminals with the `shepherd` CLI: list workspaces, tabs and panes, run a command in another pane, read output, or start, prompt and wait for another agent. Requires SHEPHERD_ENV=1."
---

# Shepherd: control panes, tabs, workspaces and agents

Shepherd organizes terminals into workspaces, tabs, and panes, recognizes coding agents running inside panes, and exposes the current session through the `shepherd` CLI. You are an agent inside one of those panes. The `shepherd-comms` skill covers messaging (bus, MCP tools, results) and `shepherd-orchestrate` covers tasks, patterns and spawning; this one covers the terminals themselves.

Before issuing any control command, verify that this agent is running inside a Shepherd-managed pane:

```bash
test "${SHEPHERD_ENV:-}" = 1
```

If the check fails, say that you are not running inside Shepherd and stop. Do not inspect or control the focused Shepherd session from outside Shepherd.

When the check passes, the `shepherd` binary in `PATH` talks to the current session. Use it to inspect neighboring work, create terminal layout, start agents and commands, read output, and wait for state changes.

## Learn the current CLI

The installed binary is the authority for command syntax. Start with:

```bash
shepherd --help
```

Then print the relevant command group by running the group without a subcommand:

```bash
shepherd agent
shepherd pane
shepherd workspace
shepherd tab
shepherd worktree
shepherd notification
shepherd integration
shepherd session
shepherd machine
```

Do not run bare `shepherd` for discovery; it launches or attaches the TUI. The Shepherd work layer is reached with `shepherd task|pattern|spawn|agents|topic ...` (see `shepherd-orchestrate`). Do not probe a mutating nested command by omitting arguments. Commands such as `shepherd workspace create` are valid with defaults and will execute.

Most control commands return JSON. Read identifiers and state from those responses instead of predicting them.

## Understand layout, panes, and agents

Choose the primitive that matches the job:

- Workspace, tab, and pane topology organize terminal locations.
- Pane commands control raw terminals, shells, tests, servers, input, and output.
- Agent commands control the recognized coding agent currently occupying a pane.

A pane exists whether or not it contains an agent. `agent start` requires an existing available shell pane and never creates, splits, or moves layout. Use pane commands for ordinary processes. Use agent commands when Shepherd must validate agent identity or interpret `idle`, `working`, `blocked`, `done`, and `unknown` lifecycle states.

Agent commands accept either a unique live agent name or the pane ID currently hosting that agent. They do not accept terminal IDs or bare agent-kind labels. Names must match `[a-z][a-z0-9_-]{0,31}` and be unique among live agents. A name follows the current pane occupant and is cleared when that agent exits, is released, or is replaced. Suspending an agent with Ctrl+Z and running a non-agent command preserves its identity while the exact process remains alive. This does not preserve the original identity across another recognized agent taking over. A retained identity does not mean the agent is in the foreground; inspect the pane before sending input.

`idle` and `done` normally indicate the agent is ready for input; neither guarantees that an unfamiliar approval dialog was recognized or that a retained agent is in the foreground. The CLI/API uses the server's seen state to distinguish them; explicit focus commands mark the target seen, while reads do not. Each TUI client tracks viewed completions independently, so its Done badge can differ from the CLI or another client's badge. Antigravity and Grok can be idle while background tasks or subagents keep running, so a settled-state wait is not proof those jobs have finished. `blocked` means Shepherd recognized an approval or question UI. `unknown` means an agent is present but Shepherd cannot classify it confidently; it does not prove completion.

## Use IDs and caller context

Public IDs are opaque stable handles:

- workspace: `w1`
- tab: `w1:t1`
- pane: `w1:p1`

Closed tab and pane IDs are not reused. A pane moved into another workspace receives a new workspace-qualified pane ID. After `pane move`, continue with `.result.move_result.pane.pane_id` or the live agent name. The old value is reported as `.result.move_result.previous_pane_id`; only the moved process's inherited caller context keeps resolving that old ID, so do not use it as a general agent target.

Shepherd injects the caller's context into each managed pane:

```bash
printf '%s\n' "$SHEPHERD_WORKSPACE_ID" "$SHEPHERD_TAB_ID" "$SHEPHERD_PANE_ID"
```

Prefer `--current` when a pane command should target the calling pane. An omitted `pane split` target uses the calling pane when `SHEPHERD_PANE_ID` is available, otherwise the focused pane. Other commands may use the UI-focused pane, which can belong to the user or another client.

Discover live state with:

```bash
shepherd workspace list
shepherd tab list --workspace "$SHEPHERD_WORKSPACE_ID"
shepherd pane current --current
shepherd pane list --workspace "$SHEPHERD_WORKSPACE_ID"
shepherd agent list
```

Creation responses expose the IDs to use next. `workspace create` returns `.result.workspace`, `.result.tab`, and `.result.root_pane`. `tab create` returns `.result.tab` and `.result.root_pane`. `pane split` returns the new pane as `.result.pane`.

## Start and coordinate an agent

Default to a sibling pane in the current tab and the current working directory. Do not create a workspace, tab, worktree, or different cwd unless the user explicitly requests that topology or location. Agents started through `shepherd spawn` or `shepherd pattern run` are different: Shepherd opens each one as a new tab of the workspace it was launched from, so do not move them.

Honor a direction requested by the user. Otherwise inspect the caller pane:

```bash
shepherd pane layout --pane "$SHEPHERD_PANE_ID"
```

Split a wide pane to the right and a narrow or tall pane down. Avoid repeated same-direction splits that create unusably narrow columns or short rows. Keep the user's focus in the calling pane and explicitly preserve the caller's working directory:

```bash
shepherd pane split --current --direction right --cwd "$PWD" --no-focus
```

Replace `right` with `down` when appropriate. Read the new pane ID from `.result.pane.pane_id`.

An available shell pane must be at its interactive prompt, with the shell itself in the foreground and no foreground command, editor, or agent running. Start a supported agent in that pane with a useful unique name:

```bash
shepherd agent start reviewer --kind codex --pane <returned-pane-id>
```

Use the kind requested by the user. Run `shepherd agent` to inspect the installed kind list and options. Pass native agent arguments only after `--`:

```bash
shepherd agent start reviewer --kind codex --pane <returned-pane-id> -- <agent-args...>
```

A successful `agent start` returns only after Shepherd detects the expected agent in the same pane and considers it ready for interactive input. If the agent is blocked during startup, the command returns `agent_not_ready` immediately but keeps the name available for `agent read` and `agent send-keys`. Wait until the agent becomes idle before prompting it. Startup defaults to a 30-second timeout.

Submit work through the agent surface:

```bash
shepherd agent prompt reviewer "Review the current diff and report only actionable findings." --wait --timeout 120000
```

`agent prompt` honors the pane's live bracketed-paste mode and sends text followed by encoded Enter as one ordered submission. It reports successful submission only after both have been written; that alone does not prove the agent started a turn. For Codex on Windows, Shepherd sends a paste boundary before Enter so submission does not depend on prompt size. It rejects an agent already waiting at an approval or question dialog with `agent_blocked` before sending any input. Inspect the blocked UI and ask the user before answering it. For normal agent work, `--wait` is enough: it waits for the first settled `idle`, `done`, or `blocked` state. Do not repeat those defaults with `--until`.

With `--wait`, a prompt sent from a non-working state must produce observed `working` or `blocked` activity. After submission, Shepherd waits up to five seconds for that activity; unrelated `idle`, `done`, or session changes do not satisfy this gate. It returns `agent_prompt_stalled` if no activity is observed, or `timeout` if the caller's timeout expires first. The caller timeout includes submission time. Without a timeout, the settled-state wait is indefinite after activity is observed. This wait tracks lifecycle state, not an individual turn; if the agent is already working, completion of the active turn may satisfy it.

Use `--until` only for a state-specific workflow, such as waiting for an already-running agent to request input:

```bash
shepherd agent wait reviewer --until blocked --timeout 120000
```

Without `--until`, standalone `agent wait` uses the same settled-state defaults as `agent prompt --wait`.

Use logical keys for interactive agent UI controls:

```bash
shepherd agent send-keys reviewer esc
shepherd agent send-keys reviewer ctrl+c
```

Shepherd validates all keys before writing any bytes. Read the result through the resolved agent:

```bash
shepherd agent get reviewer
shepherd agent read reviewer --source recent-unwrapped --lines 120
```

If a wait fails or returns `blocked`, inspect `agent get` and `agent read` before deciding what input to send. A timeout or stalled response does not prove the prompt was never delivered; do not blindly submit it again. Use the pane surface only when raw terminal control is intentional.

## Run an ordinary command in another pane

Create a sibling pane with the same geometry rule, preserve the caller's working directory, and keep user focus unchanged:

```bash
shepherd pane split --current --direction right --cwd "$PWD" --no-focus
```

Read the new pane ID from `.result.pane.pane_id`, then run and inspect the command:

```bash
shepherd pane run <returned-pane-id> "just test"
shepherd pane wait-output <returned-pane-id> --match "test result" --timeout 120000
shepherd pane read <returned-pane-id> --source recent-unwrapped --lines 120
```

`pane run` queues command text and Enter together; success acknowledges admission, not command execution. On macOS, `pane run` and `agent start` reject a command that exceeds the terminal's current canonical line-input limit before queuing it. Wait for the interactive shell prompt before retrying. The check covers this submission, not an earlier incomplete line or a later terminal-mode change. `pane wait-output` searches the selected snapshot immediately, so output that already exists can match. Use `--match <text>` for a literal substring or `--regex <pattern>` for a Rust regular expression. Omitting `--timeout` allows an indefinite wait.

Use the read source that matches the task:

- `visible`: the currently rendered viewport.
- `recent`: recent rendered output, including soft wraps.
- `recent-unwrapped`: recent output with soft wraps joined; prefer it for logs and transcripts.
- `detection`: the plain-text bottom-buffer snapshot used for agent detection.

Use `--format ansi` when colors and terminal styling are evidence. Otherwise use text.

`--lines` asks Shepherd for more rows from the pane's available screen and host scrollback. Alternate-screen rows do not enter ordinary host scrollback. For supported idle agents, Shepherd can collect application-owned history and restore the viewport afterward, but not every application or response can be recovered this way.

If a larger recent read still does not reveal the completed response, ask the agent to write it as Markdown in a temporary directory and reply only with the file path, then read that file on the same machine. Use this only as a fallback; do not request file output in the initial prompt.

## Safety and coordination rules

- Use `--no-focus` for background work unless the user asked to switch context.
- Use `--current`, an explicit pane ID, or a unique agent name. Do not rely on another client's focused pane.
- Parse IDs from JSON responses. Do not derive them from sidebar order or examples.
- Do not close workspaces, tabs, panes, or sessions you did not create unless the user explicitly asked. Ordinary CLI/API closes do not inherit the TUI's last-pane confirmation. `workspace close --group` closes the primary workspace and its linked worktree workspaces; never add it merely to bypass `workspace_group_close_required`.
- Use `--trust-repository` only after the user has verified the repository. It grants per-request Git trust; it is not a routine retry for a failed worktree command.
- Client and server versions can differ after an update. Check `shepherd status` before relying on new server features. A missing method is not permission to stop or upgrade a server.
- Never run `shepherd server stop` from an active session unless the user explicitly intends to stop the server and its pane processes. Stopping it closes every agent's terminal.
- Never kill the main Shepherd process. Use named test sessions (`shepherd --session <name>`) for experiments that need an isolated server.
- Answering a trust, login or permission dialog in another agent's pane is the human's decision: report what the pane shows instead of pressing keys.
- CLI server errors are JSON on stderr with exit status 1. CLI syntax errors exit with status 2.
