# Patterns: multi-agent teams as YAML

A **pattern** is a team of agents described in a YAML file: who takes part, with which role, vendor and model, which topics they use to talk to each other, and who starts. A **run** is one execution of a pattern with an objective.

Patterns turn "open six terminals and copy text between them" into a file you can version, validate and rerun.

## 1. Where patterns live

The first match by name wins:

1. `<repo>/.shepherd/patterns/<name>.yaml`: project, versioned with your code.
2. `~/.config/shepherd/patterns/<name>.yaml`: user-wide (`pattern new --global`).
3. Built-in templates: `orchestrator-worker`, `hierarchical`, `blackboard`, `market`. They are read-only starting points.

```
$ shepherd pattern list
NAME                 KIND                 SOURCE   SHADOWED  AGENTS  TOPICS
blackboard           blackboard           builtin  no        3       1
hierarchical         hierarchical         builtin  no        7       4
market               market               builtin  no        6       3
orchestrator-worker  orchestrator-worker  builtin  no        4       2
```

`SHADOWED` tells you when a project or user pattern hides another one with the same name.

## 2. The workflow

```bash
shepherd pattern new webteam --from orchestrator-worker   # copy a template or an existing pattern
shepherd pattern edit webteam                              # open the YAML in your editor
shepherd pattern validate webteam                          # or: --file path.yaml
shepherd pattern show webteam                              # agents and topics at a glance
shepherd pattern run webteam --task "Objective of the run"
```

```
$ shepherd pattern new webteam --from orchestrator-worker
created pattern 'webteam' at .shepherd/patterns/webteam.yaml
$ shepherd pattern validate webteam
pattern is valid
$ shepherd pattern show webteam
Name:        webteam
Kind:        orchestrator-worker
Source:      project

Agents (2):
  - orchestrator (brain)
  - worker (coder)

Topics (2):
  - orders (4 partitions)
  - results (1 partitions)
```

The terminal UI (Patterns view, `4`) and the native app offer the same steps with a gallery, a graph and an editor: pick a template with `Space`, name it, edit, validate with `v`, and run with `r`.

## 3. The YAML format

```yaml
name: webteam                     # required; letters, digits, hyphen or underscore
kind: orchestrator-worker         # orchestrator-worker | hierarchical | blackboard | market
description: Free text
topics:
- name: orders                    # required and unique
  partitions: 4                   # 1 to 256 (default 1)
  key: task_id                    # informational hint about the partition key
  filter: hypothesis              # optional: prefix expected in messages
agents:
- id: orchestrator                # required and unique
  role: brain                     # a role from your configuration (brings permissions and skills)
  vendor: claude                  # optional: replaces the role's vendor
  model: haiku                    # optional: replaces the role's model
  prompt: Instructions            # the persona; up to 16,384 characters
  count: 1                        # instances (1 to 32; at most 64 agents in total)
  level: 0                        # hierarchical only: tier in the tree
  publishes: [orders]             # topics it writes to
  consumes: [results]             # topics it reads from
  group: worker-pool              # consumer group shared by the instances
  start: true                     # this agent receives the objective and starts the run
```

Things to know:

- `prompt` **replaces** the role's working script. The role contributes only vendor, model, permissions and skills. The agent also receives its topology, its topics and the objective.
- Every `publishes` and `consumes` entry must name a declared topic.
- If no agent has `start: true`, the topology's default starter is used: the orchestrator, the director, the researcher, or the requesters.
- Agents that do not start simply wait. They are woken when a message arrives on a topic they consume.
- With `count > 1` the instances share `group`: each partition is divided among them and **each message goes to exactly one member**.
- Agent ids in a pattern (`worker`) differ from the ids the running agents get (`coder-1`, `coder-2`). Use `shepherd agents` to see the running ones.

## 4. The four topologies

### orchestrator-worker

One orchestrator splits the objective into orders and aggregates results. A pool of workers does the work.

```
 objective ─► orchestrator ──► [orders] ──► worker ×N ──► [results] ──► orchestrator
```

Rules: exactly **one** orchestrator (`count: 1`, publishes the orders) and **at least one** worker in a consumer group that consumes the orders and publishes results the orchestrator consumes.

To spread orders among workers, the orders topic needs several partitions and the orchestrator must publish **without a key** (or with different keys). With one shared key everything lands on one partition and a single worker gets it all.

```yaml
name: webteam
kind: orchestrator-worker
topics:
- {name: orders, partitions: 4, key: task_id}
- {name: results, partitions: 1}
agents:
- id: orchestrator
  role: brain
  vendor: claude
  model: haiku
  prompt: Split the objective into independent tasks, publish them to orders (without a key) and aggregate the results.
  publishes: [orders]
  consumes: [results]
  start: true
- id: worker
  role: coder
  vendor: claude
  model: haiku
  prompt: When an order arrives, do it in your worktree, commit, and publish the result to results.
  count: 2
  group: worker-pool
  publishes: [results]
  consumes: [orders]
```

### hierarchical

A tree: a director, managers and specialists. Each non-leaf node orchestrates its own subtree: it publishes orders down to the next level and reads results back up.

Rules: every agent needs a `level`; levels are contiguous from 0 with a single root at level 0; topics must not form cycles.

The template has seven agents: `director` (level 0) publishes to `root-orders`; two `manager`s (level 1) read `root-orders`, publish to `team-orders` and report to `root-results`; four `specialist`s (level 2) read `team-orders` and publish to `team-results`.

Use it for large objectives that split naturally into epics, then tasks.

### blackboard

A shared board: several agents read and write the same topic, each contributing its own kind of entry (hypotheses, validations, a synthesis).

Rules: at least two agent instances and at least one topic used (published or read) by **all** of them.

The template has a `researcher` (hypotheses), an `analyst` (validation) and a `synthesizer` (final write-up) on one `blackboard` topic whose `filter` is a message prefix such as `hypothesis:`.

Use it when the problem is exploratory and the steps are not known in advance.

### market

Requesters post asks, solvers bid, and a market-maker decides.

```
 requester ─► [ask] ─► solver ×N ─► [bid] ─► market-maker ─► [transaction] ─► winner works and reports
```

Rules: topics `ask`, `bid` and `transaction` must exist; exactly **one** market-maker that reads `ask` and `bid` and publishes `transaction`; **at least two** solvers that publish to `bid`.

Use it to compete several approaches to the same problem and keep the best.

## 5. Validation

`pattern validate` reports each problem with a severity and a field path:

```
$ shepherd pattern validate t-market
pattern has validation problems:
  [warning] agents[0].vendor: vendor 'claude' is configured but its command 'claude' is not installed or not in PATH
  [error] agents: market pattern requires at least 2 solver agent instances publishing to 'bid', found 1
```

An `error` stops a run. A `warning` informs you (for example, a vendor that is not installed, a model the vendor does not list, or a topic nobody publishes to or reads from).

What is checked:

- **Identifiers and limits:** valid, unique pattern name and agent ids; at least one agent and at most 64; `count` from 1 to 32; `partitions` from 1 to 256; persona up to 16,384 characters.
- **Topics:** every topic named in `publishes` or `consumes` exists; duplicate topic names are an error.
- **Configuration:** the `role` exists in your configuration; the vendor and model are known.
- **Topology:** the rules of the chosen `kind`, listed above.

The editors in the terminal UI and the native app show the same messages next to the field that caused them.

## 6. Running and controlling a run

```bash
shepherd pattern run webteam --task "..." [--worktree /path] [--confirm-over-limit]
shepherd pattern runs                  # ID PATTERN KIND STATE AGENTS LIVE TOPICS CREATED
shepherd pattern pause <id>
shepherd pattern resume <id>
shepherd pattern stop <id> --confirm
```

```
$ shepherd pattern run duo --task "Task #1: demo"
started pattern run run-duo-11624 for pattern duo (running) with 2 agents
$ shepherd pattern runs
ID             PATTERN  KIND        STATE    AGENTS  LIVE  TOPICS  CREATED
run-duo-11624  duo      blackboard  running  2       2     1       2026-11-09T16:38:51Z
```

What a run does:

- Starts one agent per instance with its role, vendor, model, persona, skills and topics. Roles with `worktree: true` get their own branch and worktree (`shep/<n>-run-<pattern>-...`), or all agents share one directory if you pass `--worktree`.
- Gives starters the objective as their first order and tells the others to wait for messages.
- If the run would exceed `max_concurrent` active agents, it asks for `--confirm-over-limit`.
- Never merges or pushes. Each agent leaves commits on its branch and you integrate with git.

To link a run to a task, start the objective with `Task #<id>:`. The Tasks view then shows the run and closes the task when the run finishes.

### Run states

| State | Meaning |
|---|---|
| `running` | Agents are working or waiting for messages |
| `paused` | Paused with `pattern pause` |
| `done` | Every agent is idle and nothing was published for `run_quiescence_secs` (default 30). A new message reopens the run |
| `degraded` | Some agent ended or died before the run completed |
| `failed` | Every agent failed or died |
| `stalled` | Agents are idle or blocked and the run did not finish well: an agent is blocked, or nothing was published. The reason is shown with the run |
| `stopped` | Stopped with `pattern stop` |

An agent waiting for a human answer counts as idle, so answer its questions or give it an objective that needs no approval.

## 7. Designing a good pattern

- **Start with a template** and change as little as possible. The templates already pass validation.
- **Give each agent a sharp persona.** The `prompt` is the whole job description, since it replaces the role's script. Say what to read, what to do, and what to publish where.
- **Match partitions to workers.** For N workers use a topic with at least N partitions and publish without a key.
- **Keep worktree roles few.** Each `worktree: true` agent costs a branch you will review and merge.
- **Mix vendors on purpose.** Use `vendor` and `model` per agent: a cheap model for workers, a stronger one for the orchestrator.
- **Validate before running.** A warning about a missing vendor is much cheaper to read than a run that cannot start.
- **Watch the bus.** `shepherd topic tail <topic>` shows what agents actually say to each other. See [topics-and-events.md](topics-and-events.md).

## 8. End to end example

An orchestrator and two workers add two independent features to a Go service.

```bash
cd ~/webgo

# 1) copy, adjust and validate
shepherd pattern new webteam --from orchestrator-worker
#    edit .shepherd/patterns/webteam.yaml: vendor claude, model haiku, worker count 2
shepherd pattern validate webteam

# 2) run it with an objective
shepherd pattern run webteam --task \
  "One worker adds GET /version (version.go + test); the other adds a logging middleware (logging.go + test). Each in its own worktree, not touching the other's files, and commit."

# 3) watch
shepherd pattern runs
shepherd agents
shepherd topic groups orders        # worker-pool with 2 members and the lag per partition
shepherd topic tail results         # the workers' reports

# 4) when the run is done, integrate with plain git
git worktree list
git merge --no-edit shep/6-run-webteam-189105
git merge --no-edit shep/7-run-webteam-189105
go vet ./... && go test ./...

# 5) clean up
git worktree remove ~/.shepherd/worktrees/webgo/shep-6-run-webteam-189105
git branch -d shep/6-run-webteam-189105 shep/7-run-webteam-189105
```

Each worker ends with its own branch, commit and snapshots. The two branches merge cleanly because the workers did not touch each other's files.
