---
name: status
description: "Use when Herdr sends `/status`, an agent is stalled, or you need to verify whether active work is progressing or finished. Not for inspecting or controlling Herdr panes, tabs, workspaces, or agents; use the herdr skill."
---

# Status

Optional: install this skill to use `stall_nudge_message = "/status"` instead of the default plain-text prompt.

Re-open the current objective and verify its state from source artifacts,
running processes, and checks. Do not answer from memory.

If subagents exist, poll them now. Treat a subagent whose observable work has
not advanced as stalled. Restart stalled work when it is safe to restart.

If every active task is demonstrably progressing, reply only `Working`.

If the task is finished, report completion. If its state changed but work
remains, report the change and continue the work immediately.
