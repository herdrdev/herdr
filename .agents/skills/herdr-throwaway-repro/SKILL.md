---
name: herdr-throwaway-repro
description: Create and control a disposable named Herdr session from inside an existing Herdr session. Use for isolated Herdr runtime, pane, terminal, process, API, persistence, or agent reproductions that should be driven through the CLI/API without touching the default session.
---

# Herdr throwaway reproduction

Use a disposable named Herdr session when a reproduction needs a real Herdr server, panes, PTYs, agents, or socket API without risking the user's main session.

The temporary TUI only keeps the disposable session attached and supplies terminal geometry. Drive the reproduction from the parent session through Herdr's CLI/API. Do not manually operate the nested TUI unless the bug specifically requires client input.

## Non-negotiable safety

- Never run the reproduction in the default session.
- Never stop, restart, delete, or kill the main Herdr server.
- Never use `pkill`, broad process matching, or guessed PIDs for cleanup.
- Create a unique session name. Never reuse or delete an unrelated named session.
- Create a new outer pane and close only that pane during cleanup.
- Read workspace, tab, pane, terminal, and agent IDs from command output. Never construct them.
- Use `/var/tmp` for reproduction directories and potentially large artifacts.
- Do not approve destructive or unnecessary agent actions.
- Do not spend paid agent tokens without the user's approval. Use the requested low-cost model and the smallest useful prompts.

## Learn the installed interface first

The installed binary is the authority. CLI syntax may have changed since this skill was written.

Confirm the caller is inside Herdr and inspect the relevant help before doing anything:

```bash
test "${HERDR_ENV:-}" = 1
herdr --version
herdr --help
herdr session
herdr pane
herdr agent
```

Inspect nested command help before using unfamiliar or potentially mutating commands. Do not run bare `herdr` for discovery because it launches or attaches the TUI.

Record which Herdr binary and version the reproduction tests. If testing a checkout build, follow the repository's instructions for running that build instead of silently substituting the installed binary.

## Create the outer pane

Create a sibling shell pane in the current tab without moving focus. Use an available Herdr layout tool when the harness provides one. Otherwise use the installed pane split command after checking its help.

Use `/var/tmp` or a dedicated reproduction directory as the new pane's cwd. Save the returned outer pane ID. This is the only parent-session pane that cleanup may close.

## Start the disposable session

Choose a short unique name such as `repro-<topic>-<timestamp>`.

Run the named session inside the new outer pane. Clear inherited session selection, socket overrides, and caller IDs so the nested runtime cannot accidentally address the parent session:

```bash
env \
  -u HERDR_SOCKET_PATH \
  -u HERDR_CLIENT_SOCKET_PATH \
  -u HERDR_SESSION \
  -u HERDR_WORKSPACE_ID \
  -u HERDR_TAB_ID \
  -u HERDR_PANE_ID \
  herdr --session <session-name>
```

Add reproduction-specific environment variables to this launch command when needed. Environment variables that configure the server must be present before the named server starts.

Do not continue until the named session's API is ready. Confirm readiness by addressing that session from the parent and listing its panes.

## Address only the disposable session

Every control command issued from the parent must clear inherited socket overrides and explicitly select the temporary session:

```bash
env \
  -u HERDR_SOCKET_PATH \
  -u HERDR_CLIENT_SOCKET_PATH \
  -u HERDR_WORKSPACE_ID \
  -u HERDR_TAB_ID \
  -u HERDR_PANE_ID \
  HERDR_SESSION=<session-name> \
  herdr pane list
```

Repeat this prefix for every command. Do not rely on shell state persisting between tool calls.

Read the disposable root pane ID from `pane list`. Confirm its cwd and foreground process before starting anything in it.

Named sessions isolate runtime state, sockets, panes, and persistence. They still share global Herdr configuration and agent manifest overrides by default. Check configuration provenance when it could affect the reproduction. Do not modify shared configuration merely to make the test pass.

## Drive the reproduction through the API

Use pane commands for shells and ordinary processes:

- `pane run` to start a command at an available shell prompt.
- `pane wait-output` to wait for deterministic output.
- `pane read` to capture terminal contents.
- `pane send-text` for literal input.
- `pane send-keys` for supported keys.
- `pane get`, `pane process-info`, and `pane layout` for runtime state.

Use agent commands only after Herdr recognizes a coding agent:

- `agent start` to launch a supported agent in an existing shell pane.
- `agent prompt` to submit one prompt atomically.
- `agent wait` to wait for `working`, `blocked`, `idle`, `done`, or `unknown`.
- `agent read` to capture the agent terminal.
- `agent get` and `agent explain` to inspect state and detection.
- `agent send-keys` for interactive responses.

Run the relevant command group's help first because names and options may change.

Prefer waits over arbitrary sleeps. When timing itself is under test, record timestamps and use bounded polling. Capture state before, during, and after the transition being reproduced.

When a needed terminal key is unsupported by the high-level command, send its terminal sequence through the disposable pane only after confirming the target application's expected key. Never send raw control sequences to the parent pane.

## Start agents carefully

Before launching an agent, inspect its installed `--version` and `--help`. Pass native agent arguments after Herdr's argument separator.

Use the exact model requested or approved by the user. Verify the model from the live agent screen instead of trusting an alias. Prefer low effort, safe mode, and manual permissions for a baseline when the agent supports them. Repeat with the user's real configuration only when the suspected behavior depends on hooks, plugins, or settings.

Use harmless operations for permission-state testing. Reject the pending action after evidence is captured and verify that no artifact was created.

## Collect useful evidence

Record enough information for another person to repeat the result:

- Herdr binary and version.
- Named session and launch environment.
- Target application or agent version and arguments.
- Exact commands or prompts.
- Pane and agent state before and after each transition.
- Relevant `pane read`, `agent read`, `agent explain`, API output, and session logs.
- Whether global config or a local manifest override was active.

Read the named session directory and socket from `herdr session list` instead of assuming their paths. Keep large evidence under `/var/tmp` unless the user asks to preserve it elsewhere.

Distinguish observed facts from proposed causes. First reproduce stock behavior, then change one variable at a time.

## Cleanup

Cleanup is part of the reproduction, including after failure.

1. Reject pending prompts and stop test applications cleanly when practical.
2. Verify that harmless probe files or other test artifacts do not exist, or remove only artifacts created by this reproduction.
3. Stop the temporary named session with the installed session command.
4. Delete that same stopped session.
5. Confirm it no longer appears as running.
6. Wait for the outer pane to return to its shell.
7. Close only the outer pane created by this workflow.

Never delete another named session because it looks stale. Never close the pane running the current agent or any pane not created for the reproduction.

## Report the result

State what reproduced, what did not, and the exact transition that failed. Include cleanup status. Mention shared configuration or manifest overrides that may have influenced the result.
