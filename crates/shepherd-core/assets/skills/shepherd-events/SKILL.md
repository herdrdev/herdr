---
name: shepherd-events
description: Use when collaborating via event topics, partitioned message streams, consumer groups, offsets, acknowledgments, and replays in Shepherd multi-agent patterns.
---

# Event-driven collaboration in Shepherd

Shepherd provides an event log for asynchronous and decoupled multi-agent patterns (based on "A Guide to Event-Driven Design for Agents", Confluent). Instead of direct point-to-point messages (`to`), agents publish events to partitioned topics and consume them through consumer groups with at-least-once delivery.

## When to use Topics, Keys, Groups, and Offsets

1. **Topics**: Use named topics for event categories rather than addressing agents directly.
   - Example topics: `orders`, `results`, `blackboard`, `bid`, `ask`, `transaction`.
2. **Partition Keys (`key`)**: Provide a key (e.g. `task_id`, `customer_id`, `asset_id`) when ordering matters for that entity.
   - Messages with the same key are guaranteed to route to the same partition.
   - Unkeyed messages are distributed across partitions via round-robin.
3. **Consumer Groups (`group`)**: Use consumer groups to coordinate a pool of workers.
   - Partitions are distributed deterministically across active members of the group.
   - If an agent exits or dies, the group automatically rebalances without lost or duplicated confirmed work.
   - When no group is specified, you keep the group you were already assigned on that topic (e.g. by a pattern run); only if you have none, your own agent ID is the group (pub/sub style: you receive messages from all partitions). In a pattern, do not pass `group` unless you really need a different one.
4. **Acknowledgments (`ack`)**: Confirm processed messages up through their offset.
   - `poll()` returns messages after the last confirmed offset.
   - If you crash or exit without acking, the next assigned consumer receives those messages again (at-least-once guarantee).
5. **Replay (`replay`)**: Query past events from an offset or partition without modifying group offsets or acknowledged progress.

## Four Multi-Agent Patterns

- **Orchestrator-Worker**: Orchestrator publishes tasks to `orders` (keyed by task ID). Worker pool joins consumer group `worker-pool` to pull and process tasks, acking on completion and publishing to `results`.
- **Hierarchical**: Multi-tier pipeline where managers orchestrate team topics (`team-orders`, `team-results`) and report to root topics (`root-orders`, `root-results`).
- **Blackboard**: Multiple specialists concurrently subscribe to a shared knowledge topic (`blackboard`), inspect hypotheses, and post findings.
- **Market-Based**: Requesters publish tasks to `ask`, solvers publish bids to `bid`, and a market-maker publishes matched executions to `transaction`.

## MCP Tools for Events

| Tool | Usage |
|---|---|
| `publish(topic, content, key?)` | Publish an event to `topic`. Optional `key` guarantees partition routing. |
| `subscribe(topic, group?)` | Subscribe to `topic`. Joins `group` (defaults to the group you already have on the topic, else your own ID). |
| `poll(topic, max?)` | Read unacknowledged events assigned to you in your group. |
| `ack(topic, offset?, partition?)` | Commit processed messages up through `offset`. |
| `replay(topic, from_offset, partition?, limit?)` | Inspect history from `from_offset` without affecting consumer offsets. |
| `list_topics()` | List registered topics, partition counts, and message statistics. |

## Delivery Without MCP

If running without MCP tools, events for subscribed topics are delivered directly into your terminal via:
`[shep] topic <topic>[<partition>]#<offset> from <role>#<n>: <content>`
Acknowledge by continuing your turn in the terminal.
