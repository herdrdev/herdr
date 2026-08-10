---
name: herdr-throwaway-repro
description: Create and control a disposable named Herdr session from inside an existing Herdr session. Use for isolated Herdr runtime, pane, terminal, process, API, persistence, or agent reproductions that should be driven through the CLI/API without touching the default session.
---

# Herdr throwaway reproduction

Use a disposable named Herdr session when a reproduction needs a real Herdr server, panes, PTYs, agents, or socket API without risking the user's main session.

Run that session as a headless server and drive it from the current session through Herdr's CLI/API. A headless server spawns real PTYs with no client attached, so most reproductions need no nested TUI and no extra pane in the user's session.

## Non-negotiable safety

- Never run the reproduction in the default session.
- If the disposable session cannot be started or addressed, stop and report. Never continue the reproduction in the current session, and never edit the user's `config.toml` to work around it.
- Never stop, restart, delete, or kill the main Herdr server.
- Never use `pkill`, broad process matching, or guessed PIDs for cleanup.
- Create a unique session name. Never reuse or delete an unrelated named session.
- Create a parent-session pane only when the reproduction needs an attached client, and close only that pane during cleanup.
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

## Start the disposable session

Choose a short unique name such as `repro-<topic>-<timestamp>`, then prove it is unused before launching:

```bash
herdr session list --json
```

That lists stopped sessions as well as running ones. A running name is refused, but starting a server on the name of a stopped session silently restores that session's saved workspaces and panes, and cleanup would then delete someone else's session. Pick another name on any exact match.

Start it as a headless server. `herdr --session <name>` launches the TUI, and launching the TUI from inside a Herdr pane exits with `nested herdr is disabled by default` unless the user enabled `experimental.allow_nested`. The `server` command has no such gate.

The server runs until it is stopped and never returns on its own, so start it with the harness's background primitive. The launching shell otherwise blocks here and never reaches the rest of this workflow. Clear inherited socket overrides, session selection, and caller IDs so the new server binds its own session paths instead of the parent's:

```bash
# To be run as a background job
env \
  -u HERDR_SOCKET_PATH \
  -u HERDR_CLIENT_SOCKET_PATH \
  -u HERDR_SESSION \
  -u HERDR_WORKSPACE_ID \
  -u HERDR_TAB_ID \
  -u HERDR_PANE_ID \
  herdr --session <session-name> server
```

Add reproduction-specific environment variables to this launch command when needed. Environment variables that configure the server must be present before the named server starts.

That log's first lines name the api socket, client socket, and session log. Do not continue until `herdr session list` shows the name as running; the same check catches a server that died with its launching shell.

A headless session starts empty. Create the first workspace with `herdr --session <session-name> workspace create --cwd <dir>`; the returned root pane is the reproduction's first shell. With no client attached the shared runtime size is 80x24.

### When the reproduction needs an attached client

Only bugs in client rendering, input, or attach behavior need a real TUI. That requires a nested launch, so give the nested process its own config file instead of changing the user's:

```bash
printf '[experimental]\nallow_nested = true\n' > /var/tmp/<session-name>-config.toml
```

Create a sibling shell pane in the current tab without moving focus, using an available Herdr layout tool or the installed pane split command after checking its help, with `/var/tmp` as its cwd. That split is the one command in this workflow that is meant to reach the user's session, so run it without `--session`; adding the flag would put the pane inside the disposable session, where no client can reach it. Everything that drives the disposable session still requires `--session <session-name>`.

Save the returned outer pane ID; this is the only parent-session pane that cleanup may close. Attach inside that pane using the launch environment above with `server` dropped and `HERDR_CONFIG_PATH=/var/tmp/<session-name>-config.toml` added.

Never set `experimental.allow_nested` in the user's `config.toml`.

## Address only the disposable session

Select the session with the `--session` flag on every control command:

```bash
herdr --session <session-name> pane list
```

The flag marks the session explicit, so Herdr ignores the `HERDR_SOCKET_PATH` inherited from the surrounding pane. Naming a session that is not running then fails with `server_not_running` instead of answering from the user's session.

The `HERDR_SESSION` environment variable does not do this. Inside a Herdr pane `HERDR_SOCKET_PATH` already points at the user's server and takes precedence over that variable, so `HERDR_SESSION=<session-name> herdr pane list` reads and mutates the user's session and reports success. A bare `herdr pane list` does the same. Treat any command without `--session` as aimed at the user's session.

Repeat the flag on every server-scoped command. Do not rely on shell state persisting between tool calls.

Read the disposable root pane ID from `pane list`. Confirm its cwd and foreground process before starting anything in it.

Named sessions isolate runtime state, sockets, panes, and persistence. They still share global Herdr configuration and agent manifest overrides by default. Check configuration provenance when it could affect the reproduction. Do not modify shared configuration merely to make the test pass.

## Drive the reproduction through the API

Use pane commands for shells and ordinary processes:

- `pane run <pane-id> <command>...` to start a command at an available shell prompt. The command follows the pane ID directly; an inserted `--` is typed into the shell and fails.
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
5. Confirm it no longer appears in `session list`.
6. Remove the temporary config file when one was written.
7. When an outer pane was created, wait for it to return to its shell and close only that pane.

Never delete another named session because it looks stale. Never close the pane running the current agent or any pane not created for the reproduction.

## Report the result

State what reproduced, what did not, and the exact transition that failed. Include cleanup status. Mention shared configuration or manifest overrides that may have influenced the result.
