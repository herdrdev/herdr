# Topics and the event bus

Agents in a team do not call each other. They publish messages to **topics** and read messages from topics. The bus keeps every message, so a slow or restarted agent catches up, and you can replay what happened.

This is the same model event streaming systems use, scaled down to run inside Shepherd with no extra service.

## 1. Concepts

| Term | Meaning |
|---|---|
| **Topic** | A named stream of messages, split into partitions |
| **Partition** | An ordered log inside a topic. Order is guaranteed only within one partition |
| **Offset** | The position of a message in its partition, starting at 0 |
| **Key** | Optional text. The same key always goes to the same partition |
| **Consumer group** | A set of agents that share a topic's partitions. Each message goes to exactly one member |
| **Commit (ack)** | A consumer confirms it processed messages up to an offset |
| **Lag** | How many messages a group has not committed yet |
| **Replay** | Read old messages again without moving any offsets |

Messages are persisted in Shepherd's state, so they survive restarts.

## 2. Topics

Topics of a pattern are created when the pattern runs. You can also create your own:

```
$ shepherd topic create notices --partitions 2
created topic notices (2 partitions)
$ shepherd topic list
NAME     PARTITIONS  MESSAGES
notices  2           0
```

Publishing to a topic that does not exist creates it with one partition.

## 3. Partitions and keys

```
$ shepherd topic publish notices "hello team" --from me
published message - to notices[0] (offset 0)
$ shepherd topic publish notices "second notice" --from me
published message - to notices[1] (offset 0)
```

- **Without `--key`,** messages are spread across partitions in turn. This is what you want to share work among workers.
- **With `--key`,** the same key always lands in the same partition. This keeps related messages in order (for example every message about one task).
- `--partition <p>` pins a partition explicitly.

Rule of thumb: for N parallel workers, use a topic with at least N partitions and publish without a key. If everything shares one key, one partition gets all the work and one worker does it all.

## 4. Consumer groups

Agents that declare the same `group` in a pattern divide the topic's partitions among themselves. Each message is delivered to one member of the group, so a group is a work queue. Agents in different groups each get every message, so separate groups are broadcast.

```
$ shepherd topic groups orders
TOPIC   GROUP        MEMBERS  COMMITTED  TOTAL  LAG
orders  worker-pool  2        -          2      p0:1, p1:1
```

Here two workers share `orders`; one message is waiting on each partition. When an agent subscribes without naming a group, it keeps the group the run assigned, or gets a group of its own named after its id (so it receives everything).

## 5. Reading messages

```
$ shepherd topic tail notices
[2026-11-09T16:38:40Z] notices[0]:#0 from me: hello team
[2026-11-09T16:38:40Z] notices[1]:#0 from me: second notice

$ shepherd topic tail notices --partition 0 --limit 20
$ shepherd topic replay notices --partition 0 --from 0
```

`tail` shows recent messages. `replay` rereads history from an offset and never changes what consumers have committed, so it is safe for debugging.

Add `--json` to any of these for structured output.

## 6. What agents do on the bus

Agents use six MCP tools (or the `shepherd-events` skill, which explains them):

| Tool | Purpose |
|---|---|
| `publish` | Write a message to a topic |
| `subscribe` | Join a topic (agents in a run are already subscribed to theirs) |
| `poll` | Fetch messages not yet confirmed |
| `ack` | Confirm messages up to an offset |
| `replay` | Reread history without moving offsets |
| `list_topics` | See the topics that exist |

Two details matter for agent prompts:

- **Do not pass `group` to `subscribe`** unless you mean to change the consumption group. The run already set it up.
- **Do not pass `key` or `partition` when publishing** unless ordering really matters. Without a key the bus spreads messages across partitions and therefore across the members of a group.

Agents without MCP support get messages typed into their terminal as `[shep] from <role>#<n>: ...` when their pane is idle.

### Waking idle agents

Publishing writes a short notice into the terminal of every idle subscriber except the publisher: `1 new message on 'results': use poll`. This is what makes a team event-driven: an agent can finish its turn and be woken when there is work for it, instead of polling in a loop.

A run counts as `done` when every agent is idle and nothing has been published for `run_quiescence_secs`. A new message reopens it.

## 7. Topic filters in patterns

A topic in a pattern can declare an informational `filter`, a prefix agents put at the start of a message. The blackboard template uses it so entries read `hypothesis: ...`, `validation: ...`, `synthesis: ...`. It is a convention agents follow, not a server-side rule.

```yaml
topics:
- name: blackboard
  partitions: 1
  filter: hypothesis
```

## 8. Watching the bus

- **CLI:** `shepherd topic tail <topic>` follows what agents say to each other.
- **Terminal UI, Events view (`7`):** two sub-views, switched with `Tab`. *Topics and Flow* shows live messages with offset, partition, sender, key, type and content; `Space` pauses, `r` replays from an offset, `/` filters. *Pattern Runs* shows each run, with `p` to pause or resume and `K` to stop.
- **Native app, Events view:** a feed with the time, the sender, and a color per emitter, so you can see who said what at a glance.

## 9. Patterns for using the bus well

- **Work queue:** one topic with many partitions, one consumer group of workers, publish without a key.
- **Ordered stream per item:** key by an id (`task_id`) so every message about one item stays in order.
- **Fan-out:** several agents each in their own group on the same topic, so each sees every message.
- **Request and reply:** one topic for requests, another for results, as in orchestrator-worker.
- **Shared notes:** one partition and a prefix convention, as in blackboard.

See [patterns.md](patterns.md) for the four ready-made topologies built from these pieces.
