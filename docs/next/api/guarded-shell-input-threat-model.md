# Guarded shell input threat model

Shell-mode conditional input is a narrowly scoped way to submit a resume command after an agent has exited. It must not become a generic way to type into a pane.

## Required checks

- Bind the request to the current pane, workspace, terminal, and a fresh detection-snapshot observation token.
- Require a local PTY runtime with a live shell whose PID is the foreground process-group ID and the sole foreground-group member.
- Require no descendant processes, including background children that are outside the foreground group.
- Require the shell process cwd to equal the caller's absolute cwd exactly.
- Refuse when agent identity/session metadata is present, process inspection is unavailable, or any foreground member is unknown or differs from the expected shell.
- Perform process validation after token validation while holding the runtime input-admission lock, immediately before the write. Agent-mode behavior stays unchanged.

## Abuse fixtures

| Fixture | Expected result |
| --- | --- |
| Agent session or effective agent present | Refuse; no bytes written |
| Expected PID differs from pane shell PID | Refuse |
| Foreground PGID differs from the shell PID | Refuse |
| Foreground group contains another process | Refuse |
| Expected cwd differs from live shell cwd | Refuse |
| Workspace, terminal, pane, or host identity differs | Refuse |
| Observation token is stale or input remains unreflected | Refuse |
| Process identity/cwd changes after observation but before admission | Refuse |
| Remote proxy, unavailable process data, or unknown executable | Refuse |
| All identities, process facts, cwd, and token match | Send once; consume the observation |


