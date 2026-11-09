# Agents, roles, vendors and skills

How an agent lives inside Shepherd, how to choose who does what, and how to teach agents your conventions.

## 1. Agent lifecycle

`shepherd spawn <role> <task>` (or `s` in the Agents view, or a pattern run) does this:

1. Resolves the **role** into a vendor, a model and a fallback, using the configuration.
2. Checks the limits (`max_concurrent`, `max_depth`).
3. If the role has `worktree: true`, creates a `git worktree` and a branch `shep/<n>-<task>` under `~/.shepherd/worktrees/<repo>/`.
4. Installs the role's skills and starts the vendor in a terminal pane, with Shepherd's MCP server injected and a token of its own.
5. Sends the first prompt: the task, or the persona defined in a pattern.

States: `starting`, then `working` and `idle` alternating. `blocked` means a person is needed (a trust dialog, a login, a question). `done` means it delivered a result. `killed` and `exited` are final.

```
$ shepherd agents
ID            STATE     ROLE        VENDOR  MODEL  TASK
specialist-1  starting  specialist  codex   -      Task-1-demo
```

- **Open its terminal:** `shepherd attach <agent>` (or `Enter` in the Agents view). That is where you answer permission prompts and questions.
- **Clean up:** a finished agent is removed with `x` in the Agents view. Its snapshots are kept according to `max_per_agent`.
- **Agents without MCP:** bus messages are typed into their terminal as `[shep] from <role>#<n>: ...` when the pane is idle.
- **Inherited configuration:** agents inherit your global vendor configuration (for example `~/.claude/CLAUDE.md`). If it demands plan approval before editing, the agent will wait. Include the approval in the task.

`shepherd spawn` accepts `--vendor` and `--model` to override a role for one agent, and `--task-id` to link the agent to a task.

## 2. Roles

A role is a named job description. It is not a vendor: it says which vendor and model are the default for that job, plus permissions and skills.

| Role | Default vendor | Purpose | Own worktree | May start agents |
|---|---|---|---|---|
| `brain` | claude / opus | Plan, split work, aggregate results | no | yes |
| `coder` | codex (fallback gemini, claude) | Implement | yes | no |
| `reviewer` | gemini (must differ from the coder's vendor) | Review code and security | no | no |
| `docs` | gemini | Documentation and docs questions | no | no |
| `tester` | claude / sonnet | Tests and debugging | no | no |

See what your configuration resolves to:

```
$ shepherd roles list
brain      claude/opus
coder      codex/default
docs       gemini/default
reviewer   gemini/default
tester     claude/sonnet
```

### Changing a role or adding one

Roles live in `config.yaml`, globally (`~/.config/shepherd/config.yaml`) or per project (`<repo>/.shepherd/config.yaml`). Write only what changes; everything else comes from the defaults, and maps merge deeply (a `null` value resets a key).

```yaml
roles:
  coder:
    vendor: claude          # instead of codex
    model: sonnet
    worktree: true
  specialist:               # a new role with any name
    vendor: codex
    worktree: false
    prompt: You focus on database migrations.
    skills: [lang-go, debug]
```

| Field | Meaning |
|---|---|
| `vendor` | Which vendor runs the role |
| `model` | Model; empty means the vendor's default |
| `fallback` | Alternative vendors if the main one is not available |
| `must_differ_from` | The vendor must differ from another role's (cross-review) |
| `worktree` | The agent works in its own worktree and branch |
| `can_spawn` | The agent may start sub-agents (the `spawn_agent` tool) |
| `skills` | Skills specific to this role |
| `prompt` | Your own instructions for the role |

Check the result with `shepherd config validate`. Common problems it reports: a role pointing at an unknown vendor or fallback, a `must_differ_from` that names an unknown role, or limits below 1.

In the native app, the **Models** view lists the agents and models installed on your machine, grouped by vendor, and lets you pick them for each role without editing YAML.

## 3. Vendors

Shepherd drives agent CLIs. Three are built in:

| Vendor | Command |
|---|---|
| `claude` | `claude` |
| `codex` | `codex` |
| `gemini` | `agy` |

```
$ shepherd vendors list
claude   claude   available
codex    codex    available
gemini   agy      available
```

A vendor entry in `config.yaml` describes how to start it and how to reach it:

| Field | Meaning |
|---|---|
| `command`, `args` | Executable and startup arguments |
| `models`, `default_model`, `model_flag` | Available models, the default, and the flag that selects one |
| `print_args` | Arguments of the non-interactive mode |
| `mcp.supported`, `mcp.style` | Whether MCP is supported and how it is injected |
| `skills.mode`, `skills.style`, `skills.file` | How skills are delivered (`native`, `instructions`, `prompt`) |
| `shepherd_agent` | The name the agent detector knows it by |
| `disabled` | Excludes the vendor; roles fall back to their `fallback` |

To switch a vendor off: `vendors: { gemini: { disabled: true } }`.

## 4. Skills

A skill is a folder with a `SKILL.md`: instructions that Shepherd installs into an agent according to its role. Each vendor receives them its own way (a plugin for claude, developer instructions for codex, an injected prompt for gemini), so you write them once.

### Built-in skills

They are protected from deletion.

| Skill | Use |
|---|---|
| `plan-tasks` | Turn a task into independent subtasks with acceptance criteria |
| `implement-task` | Small focused changes in the agent's worktree |
| `code-review` | Verify criteria and answer APPROVE or REQUEST_CHANGES |
| `security-review` | Secrets, injection, validation, auth, unsafe file or process handling |
| `write-tests` | Table-driven cases, edge cases |
| `debug` | Reproduce, isolate, root cause, regression test |
| `conventional-commits` | Commit message format |
| `pr-workflow` | Branch to pull request |
| `docs-lookup` | Check official docs before assuming behavior |
| `lang-go`, `lang-python`, `lang-ts` | Language conventions, activated automatically by repository language |
| `shepherd-comms` | The MCP tools for messaging, spawning and delivering results |
| `shepherd-events` | Topics, partitions, consumer groups, offsets |
| `shepherd-control` | Controlling terminals with the `shepherd` CLI |
| `shepherd-orchestrate` | Planning and running work with Shepherd safely |

`shepherd skills list` shows each with its tier.

### Writing your own

Create a folder with a `SKILL.md`. The folder name must match the `name` in the front matter.

```markdown
---
name: api-conventions
description: Use when changing HTTP handlers - routing, error format and status codes.
---

# API conventions

- Return errors as `{ "error": "<message>" }` with the right status code.
- Never log request bodies.
```

The `description` is one line telling the agent when to use it. Start it with "Use when".

Where it can live:

| Level | Path |
|---|---|
| Project | `<repo>/.shepherd/skills/<name>/SKILL.md` |
| User | `~/.config/shepherd/skills/<name>/SKILL.md` |
| Global (every agent) | a `_global/` folder inside either location, or `skills.global` in `config.yaml` |

Priority is repository, then user, then built-in. A project skill with the same name as a built-in one overrides it, and the original can be restored.

### Assigning skills

There are three levels, combined when an agent starts:

1. **Global:** `skills.global: [lang-go]` installs a skill in every agent.
2. **Role:** `roles.<role>.skills: [name, ...]` installs it in agents with that role.
3. **Session:** adjustments for one agent.

`skills.disabled: [name]` excludes a skill. If another layer shadows a change you try to make, Shepherd tells you which layer wins instead of silently ignoring it.

```yaml
skills:
  global: [conventional-commits]
  disabled: [lang-ts]
roles:
  reviewer:
    skills: [code-review, security-review]
```

**Timing matters:** editing a skill's text reaches a running agent immediately (claude and codex). Adding or removing a skill takes effect only when the agent is restarted.

### Managing skills in the interfaces

- **Terminal, Skills view (`5`):** `c` switches between catalog and per-agent, `Space` enables or disables, `n` creates, `g` makes global, `d` deletes, `r` restores the original of an edited built-in.
- **Native app, Skills view:** an editor with line numbers to create, edit and delete skills. A dialog in the role settings lets you assign existing skills with a few clicks.

`shepherd config validate` flags a skill without front matter, with a name that differs from its folder, or one a role references that does not exist.

## 5. MCP tools an agent sees

Agents with MCP support see these as tools of the `shepherd` server (a local HTTP server, with one token per agent).

| Tool | What it does |
|---|---|
| `spawn_agent` | Start a sub-agent (only roles with `can_spawn: true`) |
| `send_message`, `read_messages` | Direct message to another agent by id or role; read and drain pending ones |
| `wait_for` | Wait for given agents to deliver a result or exit |
| `submit_result`, `get_result` | Deliver the final result; read another agent's without blocking |
| `list_agents` | Agents in the session with role, vendor, state and worktree |
| `get_diff` | Full diff of a coder's worktree against the base branch |
| `read_output` | Last N lines of an agent's pane |
| `ask_docs` | Ask the `docs` agent (started if needed) and wait |
| `get_task_context`, `update_task_context` | Read the task (issue, spec, criteria, plan) or, for `brain` and `docs` only, update it. In pattern runs it returns the objective and the topology |
| `get_workspace_instructions` | Repository instructions, effective skills, detected checks |
| `publish`, `subscribe`, `poll`, `ack`, `replay`, `list_topics` | The event bus, see [topics-and-events.md](topics-and-events.md) |

Limits protect you from runaway trees: `max_depth` bounds how deep agents may start agents, and `max_concurrent` bounds how many work at once.

## 6. The task workflow: `shepherd run`

For a task in `.shepherd/tasks`:

1. **Plan:** the `brain` splits the task into subtasks (`task plan <id> --save` stores it). With `workflow.confirm: true` it waits for `shepherd approve <id> plan`.
2. **Implement:** one `coder` per subtask, each in its own worktree.
3. **Review:** up to `max_review_rounds` rounds with a `reviewer` from another vendor.
4. **Checks:** your `workflow.checks`, or ones detected from the language.
5. **Publish:** push and a draft PR with `gh`, only after you approve (`approve <id> <n>`) or with `--auto-push`. Protected branches (`main`, `master`, `develop`) are never published from.

Reject with `shepherd reject <id> plan|<n> --reason "..."`. Follow progress with `shepherd runs list` or the Runs view.

```yaml
workflow:
  max_review_rounds: 3     # 0 disables review
  confirm: true            # require plan approval
  base_branch: main
  checks: ["go vet ./...", "go test ./..."]
  draft: true
```

## 7. Snapshots

Shepherd photographs an agent's worktree on its own: when the agent goes idle, when it finishes, and every `interval_secs` while it works. You can also take one by hand. Before a destructive action (restore, kill) it saves a `pre-restore` or `pre-kill` snapshot first.

```bash
shepherd snapshot capture coder-1
shepherd snapshot list --agent coder-1
shepherd snapshot diff coder-1/1                      # against the current state
shepherd snapshot restore coder-1/1 --confirm         # saves 'pre-restore' first
shepherd snapshot restore coder-1/1 --path main.go --confirm
shepherd snapshot prune --agent coder-1 --keep 10
```

Snapshots live in `refs/shepherd/snapshots/<agent>/<n>` inside your repository. They do not touch your branch or your index.
