# Shepherd Event-Driven Multi-Agent Patterns (Phase F8a)

Based on *A Guide to Event-Driven Design for Agents and Multi-Agent Systems* (Confluent, 2025), Chapter III.

## Overview

Shepherd Multi-Agent Patterns provide structured topologies for coordinating teams of AI agents via an append-only event log with named topics, consumer groups, and partition keys. Instead of fragile point-to-point agent invocations, agents communicate exclusively through event streaming.

Patterns can be stored at two user levels plus built-in defaults:
1. **Project-local**: `.shepherd/patterns/<name>.yaml` (highest precedence, versioned with git)
2. **User-global**: `$XDG_CONFIG_HOME/shepherd/patterns/<name>.yaml` (fallback `~/.config/shepherd/patterns/<name>.yaml`)
3. **Built-in templates**: embedded in Shepherd binary as immutable starting points.

## Supported Patterns

### 1. `orchestrator-worker`
A central orchestrator breaks complex tasks into orders and publishes them to a partitioned topic with partition keys (e.g. `task_id`). A worker pool consumes from the orders topic as a consumer group and publishes completed deliverables to a results topic, which the orchestrator aggregates.

```yaml
name: orchestrator-worker
kind: orchestrator-worker
description: Central orchestrator dispatches partitioned tasks to a worker pool and aggregates results.
topics:
  - name: orders
    partitions: 4
    key: task_id
  - name: results
    partitions: 1
agents:
  - id: orchestrator
    role: brain
    count: 1
    prompt: You are the lead orchestrator. Analyze requests, partition tasks across workers, and aggregate incoming results into completed deliverables.
    publishes:
      - orders
    consumes:
      - results
  - id: worker
    role: coder
    count: 3
    group: worker-pool
    prompt: You are an autonomous task worker. Read assignments from the orders topic, execute the work cleanly, and post verified results to results topic.
    consumes:
      - orders
    publishes:
      - results
```

### 2. `hierarchical`
Recursive orchestration forming a tree structure (director → managers → specialists). Each non-leaf node acts as orchestrator for its immediate subtree, publishing orders down to level $N+1$ and consuming results back up from level $N+1$.

```yaml
name: hierarchical
kind: hierarchical
description: Multi-tier command hierarchy where managers supervise specialized worker teams.
topics:
  - name: root-orders
    partitions: 2
    key: epic_id
  - name: root-results
    partitions: 1
  - name: team-orders
    partitions: 4
    key: subtask_id
  - name: team-results
    partitions: 2
agents:
  - id: director
    role: brain
    level: 0
    count: 1
    prompt: You are the director. Decompose system epics into milestone directives and review synthesized outcomes from engineering managers.
    publishes:
      - root-orders
    consumes:
      - root-results
  - id: manager
    role: reviewer
    level: 1
    count: 2
    group: managers
    prompt: You are an engineering manager. Convert directives into team assignments, guide specialist workers, and report completed milestones upward.
    consumes:
      - root-orders
      - team-results
    publishes:
      - root-results
      - team-orders
  - id: specialist
    role: coder
    level: 2
    count: 4
    group: specialists
    prompt: You are a specialist engineer. Implement assigned technical tasks thoroughly and publish results to the team results topic.
    consumes:
      - team-orders
    publishes:
      - team-results
```

### 3. `blackboard`
A shared knowledge base where multiple specialized agents communicate asynchronously by posting hypotheses, discoveries, or evaluations to a shared topic (with optional subject filters), and subscribing to entries of interest.

```yaml
name: blackboard
kind: blackboard
description: Shared knowledge base where specialist agents read hypotheses and post discoveries.
topics:
  - name: blackboard
    partitions: 1
    filter: hypothesis
agents:
  - id: researcher
    role: brain
    count: 1
    prompt: You formulate problem hypotheses and read updates from specialist agents on the blackboard.
    publishes:
      - blackboard
    consumes:
      - blackboard
  - id: analyst
    role: tester
    count: 1
    prompt: You inspect blackboard assertions for logical flaws, test edge cases, and post validation reports.
    publishes:
      - blackboard
    consumes:
      - blackboard
  - id: synthesizer
    role: docs
    count: 1
    prompt: You synthesize verified facts on the blackboard into cohesive technical documentation.
    publishes:
      - blackboard
    consumes:
      - blackboard
```

### 4. `market`
A dynamic market with `bid`, `ask`, and `transaction` topics. Solvers submit bids for requested work, requesters post asks, and an impartial market-maker matches matching pairs and emits binding settlement transactions.

```yaml
name: market
kind: market
description: Competitive market where solvers bid on asks coordinated by a market-maker.
topics:
  - name: bid
    partitions: 2
  - name: ask
    partitions: 2
  - name: transaction
    partitions: 1
agents:
  - id: market-maker
    role: brain
    count: 1
    prompt: You match task asks with solver bids fairly and emit binding transaction events to settle assignments.
    consumes:
      - bid
      - ask
    publishes:
      - transaction
  - id: requester
    role: docs
    count: 2
    group: requesters
    prompt: You post requests as asks on the market topic and listen for confirmed transactions to receive work.
    publishes:
      - ask
    consumes:
      - transaction
  - id: solver
    role: coder
    count: 3
    group: solvers
    prompt: You inspect asks on the market, submit competitive bids with cost and latency estimates, and execute winning work.
    publishes:
      - bid
    consumes:
      - ask
      - transaction
```

## Schema Specification

### `Pattern`
- `name` (String, required): Slug identifier (`[a-zA-Z0-9_-]+`).
- `kind` (Enum, required): `orchestrator-worker`, `hierarchical`, `blackboard`, `market`.
- `description` (String, required): Human-readable purpose of the pattern.
- `topics` (Array of Topic): Topics created for inter-agent communication.
- `agents` (Array of PatternAgent): Agents participating in this pattern.

### `Topic`
- `name` (String, required): Topic identifier.
- `partitions` (u32, default: 1): Number of partitions for parallel worker routing.
- `key` (String, optional): Payload field used as partition hash key (e.g. `task_id`).
- `filter` (String, optional): Tag or header filter criteria (e.g. for blackboard topics).

### `PatternAgent`
- `id` (String, required): Unique agent identifier in the pattern.
- `role` (String, required): References a configured role in `.shepherd/config.yaml` or global config (`brain`, `coder`, `reviewer`, `tester`, `docs`).
- `vendor` (String, optional): Overrides the role's default vendor (`claude`, `codex`, `gemini`).
- `model` (String, optional): Overrides the role's default model (e.g. `sonnet`, `haiku`, `opus`).
- `prompt` (String, optional): Custom agent persona instruction (max 16,384 characters).
- `count` (u32, default: 1): Number of concurrent agent instances in this role.
- `level` (u32, optional): Hierarchy tier (0 = top director, 1 = mid manager, 2 = worker leaf).
- `publishes` (Array of String, default: `[]`): Names of topics published to.
- `consumes` (Array of String, default: `[]`): Names of topics consumed from.
- `group` (String, optional): Consumer group identifier for competing-consumer load balancing.

## Validation Rules

Validation produces a list of `Problem` objects with `severity` (`"error"` or `"warning"`), `path` (e.g. `agents[0].role`), and `message`:

1. **Identifiers & Limits**:
   - Unique, valid pattern name and agent IDs.
   - At least 1 agent, maximum 64 agent declarations.
   - Agent count between 1 and 32.
   - Topic partitions between 1 and 256.
   - Agent persona prompt length $\le$ 16,384 chars.
2. **Topic Integrity**:
   - All published/consumed topics must exist in `pattern.topics`.
   - Topics that are neither published nor consumed generate a `"warning"` (orphan topic).
   - Duplicate topic names generate an `"error"`.
3. **Configuration & Vendor Alignment**:
   - `agent.role` must exist in `config.roles` (`"error"` if missing).
   - Vendor must be in `config.vendors` and present on `$PATH` (`"warning"` if missing or not installed).
   - If model is overridden, it must be recognized in the vendor's models list (`"warning"` if unlisted).
4. **Pattern-Specific Topology**:
   - **Orchestrator-Worker**: Exactly 1 orchestrator + $\ge 1$ worker with a consumer group + response topic consumed by orchestrator and published by workers.
   - **Hierarchical**: Directed tree without cycles, single root at level 0, contiguous levels ($0, 1, \dots, N$), each non-leaf node orchestrating down to its sub-level.
   - **Blackboard**: $\ge 2$ agents sharing at least one common topic used by all participants.
   - **Market**: 1 market-maker + $\ge 2$ solver agents + `bid`, `ask`, and `transaction` topics.
