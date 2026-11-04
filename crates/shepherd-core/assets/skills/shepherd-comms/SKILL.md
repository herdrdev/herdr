---
name: shepherd-comms
description: Use when you are an agent running under shepherd and need to spawn sub-agents, message other agents, wait for results, or deliver your own result. Explains the shepherd MCP tools and the fallback for agents without MCP.
---

# Working with other agents through shepherd

You run inside a shepherd session. Your id is in `$SHEP_AGENT_ID`. Other agents run in their own shepherd tabs.

## Tools (MCP server `shepherd`)

| Tool | Use it to |
|---|---|
| `spawn_agent(role, task, vendor?, model?)` | start a sub-agent in a new tab; returns `agent_id`. Only if your role has `can_spawn`. |
| `send_message(to, content)` | leave a message for an agent (`to` = id or role). |
| `read_messages()` | read your pending messages. Check it when you start, and after every long step. |
| `wait_for(agent_id, timeout)` | block until that agent finishes. |
| `submit_result(summary)` | deliver YOUR final result. Call it exactly once, when done. |
| `get_result(agent_id)` | read another agent's final result. |
| `list_agents()` | see who is running, with role, vendor and status. |
| `get_diff(agent_id)` | the diff of a coder's worktree. |
| `read_output(agent_id, lines)` | recent terminal output of another agent. |
| `ask_docs(question)` | ask the docs agent and wait for the answer. |
| `get_task_context(section?)` | read issue, spec, acceptance criteria. |
| `update_task_context(section, content)` | add findings (brain and docs only). |
| `get_workspace_instructions()` | re-read workspace rules if you lost them. |

## Rules

1. **Start** by calling `get_task_context()` and `read_messages()`.
2. **Delegate precisely**: a task for a sub-agent states the goal, the files in scope, and how to verify. Never say "figure it out".
3. **Do not poll in a loop.** Use `wait_for` with a timeout, then decide.
4. **Finish with `submit_result`**: what you did, what you verified (commands and results), what is left. Not a transcript.
5. If blocked, `send_message` to the brain (or the user's agent) with the exact question; do not guess.
6. Never spawn more agents than needed; limits on depth and concurrency are enforced and may require human approval.

## Without MCP

If the shepherd tools are not available, messages arrive in your terminal as `[shep] from <role>#<n>: ...`. Answer in the terminal. When you finish, state your result clearly and stop; shepherd reads it when your status becomes `done`.
