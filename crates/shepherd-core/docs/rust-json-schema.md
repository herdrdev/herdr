# Shepherd Rust CLI JSON Schemas (`--json`)

This document specifies the stable JSON output schemas emitted by the Rust CLI (`shepherd`) when invoked with `--json` on:

- `shepherd agents --json`
- `shepherd task list [--archived] --json`
- `shepherd task show <id-or-name> [section] --json`
- `shepherd roles list --json`

## Security Invariant: No Agent Tokens in JSON Output

`shepherd agents --json` (as well as the `list_agents` MCP tool) projects every agent through `Agent::public()`, which clears `token`, and `Agent.token` is annotated with `#[serde(default, skip_serializing_if = "String::is_empty")]`. Consequently, neither the bearer token value nor the `"token"` key ever appears in CLI JSON output.

---

## 1. `shepherd agents --json`

Returns a JSON array of public `Agent` objects (`[]` when no agents are registered).

### Schema (`Agent[]`)

| Field | Type | Presence | Description |
|---|---|---|---|
| `id` | `string` | required | Shepherd agent identifier (e.g. `"coder-1"`). |
| `seq` | `integer (u64)` | required | Monotonic sequence number in `agents.json`. |
| `role` | `string` | required | Role name (`"brain"`, `"coder"`, `"reviewer"`, `"docs"`, `"tester"`, etc.). |
| `vendor` | `string` | required | Assigned vendor (`"claude"`, `"codex"`, `"gemini"`, etc.). |
| `model` | `string` | omitted if empty | Assigned model identifier. |
| `task_id` | `string` | omitted if empty | Associated task ID or reference. |
| `task_text` | `string` | omitted if empty | Task description or prompt summary given at spawn. |
| `parent_id` | `string` | omitted if empty | Spawning parent agent ID, if any. |
| `depth` | `integer (u32)` | required | Spawn nesting depth (`0` for root agents). |
| `root` | `string` | required | Project root path. |
| `mcp` | `boolean` | required | Whether the agent was launched with Shepherd MCP configured. |
| `skills` | `object` | required | Per-session skill adjustments (`{"add": string[], "remove": string[]}`, empty arrays omitted). |
| `active_skills` | `string[]` | omitted if empty | Effective skill names installed for this agent session. |
| `workspace` | `string` | omitted if empty | Terminal backend workspace identifier. |
| `tab` | `string` | omitted if empty | Terminal backend tab or terminal identifier. |
| `pane` | `string` | omitted if empty | Terminal backend pane or terminal address (`w1:p2`, `t7`, etc.). |
| `label` | `string` | omitted if empty | Human-readable pane/tab label (`role·vendor·slug`). |
| `worktree` | `string` | omitted if empty | Git worktree path if the role runs in an isolated worktree. |
| `branch` | `string` | omitted if empty | Git branch name in the worktree (`shep/<seq>-<slug>`). |
| `state` | `string` | required | Lifecycle state (`"starting"`, `"working"`, `"idle"`, `"blocked"`, `"done"`, `"exited"`, `"killed"`, `"failed"`). |
| `result` | `string` | omitted if empty | Final result summary submitted via `submit_result`. |
| `error` | `string` | omitted if empty | Error message when `state` is `"failed"`. |
| `notice` | `string` | omitted if empty | Operational notice (e.g. when blocked waiting for human input). |
| `has_result` | `boolean` | required | `true` once `submit_result` has been recorded. |
| `started` | `string` | required | RFC 3339 creation timestamp. |
| `updated` | `string` | required | RFC 3339 last-update timestamp. |

> **Redacted field**: `token` is **never** present in `shepherd agents --json`.

### Example

```json
[
  {
    "id": "coder-1",
    "seq": 1,
    "role": "coder",
    "vendor": "codex",
    "model": "gpt-5",
    "task_id": "1",
    "task_text": "Implement JSON output",
    "depth": 0,
    "root": "/repo",
    "mcp": true,
    "skills": {},
    "pane": "w1:p1",
    "state": "starting",
    "has_result": false,
    "started": "2026-11-03T10:00:00Z",
    "updated": "2026-11-03T10:00:00Z"
  }
]
```

---

## 2. `shepherd task list [--archived] --json`

Returns a JSON array of `TaskListEntryJson` objects (`[]` when no tasks match), sorted by ascending task `id`. Each entry flattens the Go-compatible `TaskStatus` (`status.json`) and adds `directory`.

### Schema (`TaskListEntryJson[]`)

| Field | Type | Presence | Description |
|---|---|---|---|
| `id` | `integer (u32)` | required | Numeric task ID. |
| `slug` | `string` | required | Normalized ASCII slug. |
| `title` | `string` | required | Task title. |
| `state` | `string` | required | Task state (`"new"`, `"planning"`, `"coding"`, `"review"`, `"awaiting-approval"`, `"pr"`, `"done"`, `"needs-human"`, `"archived"`). |
| `issue` | `object` | omitted if `null` | Linked issue metadata (`{"number": u32, "url": string, "repo"?: string}`). |
| `branch` | `string` | omitted if empty | Task branch name, if set. |
| `pr` | `integer (u32)` | omitted if `0` | Pull request number, if set. |
| `subtasks` | `Subtask[]` | omitted if empty | Parsed/scheduled subtasks (`n`, `title`, `body?`, `depends?`, `verify?`, `state`, `note?`, `coder?`, `reviewer?`, `round?`, `branch?`, `pr?`). |
| `waiting` | `string` | omitted if empty | Human gate name when waiting for approval (e.g. `"plan"`). |
| `note` | `string` | omitted if empty | Latest workflow status note. |
| `agents` | `string[]` | omitted if empty | IDs of agents spawned for this task. |
| `created` | `string` | required | RFC 3339 creation timestamp. |
| `updated` | `string` | required | RFC 3339 last-update timestamp. |
| `directory` | `string` | required | Path to the task directory on disk (`.shepherd/tasks/<id>-<slug>`). |

---

## 3. `shepherd task show <id-or-name> [section] --json`

Supports two forms: full task inspection (when `section` is omitted) and single-section inspection (when `section` is provided, e.g. `spec`, `plan`, `notes`, `decisions`, `issue`).

### 3a. Full Task View (`shepherd task show <id-or-name> --json`)

Returns a `TaskDetailJson` object flattening `TaskStatus` plus `directory`, `sections`, and `context`:

| Field | Type | Presence | Description |
|---|---|---|---|
| *(all `TaskListEntryJson` fields)* | — | required / optional | Same fields as `TaskListEntryJson` (`id`, `slug`, `title`, `state`, `directory`, etc.). |
| `sections` | `Record<string, string>` | required | Map of existing task markdown sections (`"issue"`, `"spec"`, `"acceptance"`, `"links"`, `"plan"`) to their raw markdown contents. |
| `context` | `string` | required | Combined markdown task context (same text returned by `get_task_context`). |

### 3b. Single Section View (`shepherd task show <id-or-name> <section> --json`)

Returns a `TaskSectionJson` object:

| Field | Type | Presence | Description |
|---|---|---|---|
| `id` | `integer (u32)` | required | Numeric task ID. |
| `slug` | `string` | required | Task slug. |
| `section` | `string` | required | Requested section name (`"spec"`, `"plan"`, `"notes"`, `"decisions"`, or `"issue"`). |
| `path` | `string` | required | Filesystem path to `<section>.md`. |
| `content` | `string` | required | Raw markdown content of the section. |

---

## 4. `shepherd roles list --json`

Returns a JSON array of `RoleListEntryJson` objects sorted lexicographically by `role`.

### Schema (`RoleListEntryJson[]`)

| Field | Type | Presence | Description |
|---|---|---|---|
| `role` | `string` | required | Role identifier (`"brain"`, `"coder"`, `"docs"`, `"reviewer"`, `"tester"`, etc.). |
| `configured_vendor` | `string` | required | Primary vendor configured for the role before routing/fallbacks. |
| `configured_model` | `string` | omitted if empty | Primary model configured for the role. |
| `vendor` | `string` | omitted if `available == false` | Resolved active vendor selected for the role. |
| `model` | `string` | omitted if `available == false` | Resolved model selected for the role. |
| `fallback` | `boolean` | required | `true` when the resolved vendor/model came from a fallback or single-vendor independence rule. |
| `available` | `boolean` | required | `true` when at least one valid vendor is available for this role. |
| `error` | `string` | omitted if `available == true` | Explanation of why the role could not be routed when `available == false`. |
| `worktree` | `boolean` | required | Whether agents of this role run in an isolated git worktree by default. |
| `can_spawn` | `boolean` | required | Whether agents of this role are permitted to call `spawn_agent`. |
| `skills` | `string[]` | omitted if empty | Configured skill names for this role. |
