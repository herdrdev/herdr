# Guarded agent interactions (prerequisite)

The JSON runtime API adds `agent.interaction.get`, `agent.interaction.submit`,
and `agent.interaction.receipt`. These methods are independent from raw
`agent.send_keys` / `pane.send_text`, whose guarantees remain unchanged.
This prerequisite ships **no verified native dialog profile**. `get` reports
`supported: false`; typed submits are durably rejected without terminal input.
It does not yet provide working automated answers for Codex or Claude.

## Observation

`get` accepts `{ "target": "<terminal id or agent target>" }`. Its result type
is `agent_interaction`, with an `observation`, `supported`, and
`unsupported_reason`. Observation requires every field below:

```json
{
  "terminal_id": "<exact terminal id>",
  "server_instance_id": "<process incarnation>",
  "runtime_pid": 123,
  "agent": "codex",
  "agent_session": {"source":"<source>","agent":"codex","kind":"id","value":"<session id>"},
  "state_change_seq": 42,
  "content_digest": "<sha256 of full UTF-8 detection buffer>"
}
```

The digest covers the complete bottom-buffer detection text returned by the
runtime, without line limit, trimming or normalization. It is not a semantic
dialog digest. Missing runtime, agent or session identity returns an error.
Process incarnation makes observations stale after cold restart or live handoff;
runtime PID detects PTY replacement within one server process.

## Submit and payload binding

`submit` accepts `operation_id`, `payload_digest`, `expected` (the exact
observation), and one typed `action`:

```json
{"type":"choose","option_id":"<native option id>"}
{"type":"free_text","text":"<answer>"}
{"type":"begin_custom","option_id":"<native custom-entry option id>"}
{"type":"submit_custom","text":"<answer>","parent_operation_id":"<begin operation>"}
```

Unknown action/parameter fields and raw keys are rejected. Operation IDs contain
1..128 ASCII letters, digits, underscores or hyphens. Compute `payload_digest`
as lowercase SHA-256 of compact UTF-8 serde JSON for
`[operation_id, expected, action]`. Object key order must match the Rust
structures (JSON Schema describes types but does not prescribe key order): observation keys appear in the order shown above; action
starts with `type`, then `option_id`, or `text`, or `text` followed by
`parent_operation_id`. Preserve strings exactly; do not normalize answers.

The serialized app handler checks terminal/process/agent/session identity,
`state_change_seq`, and current buffer digest before profile preparation and queue
submission. All current profiles return `unsupported_interaction_profile`.
Client-selected option IDs are not evidence of an actual selectable affordance.
Custom-phase lineage validation and native affordance recognition are required
before a profile can implement the currently reserved custom actions.

## Persistent receipts and recovery

`receipt` accepts `{ "operation_id": "<original operation id>" }`. Submit and
receipt return result type `agent_interaction_receipt` with `receipt` containing
`operation_id`, `payload_digest`, `outcome`, and optional `code`:

- `rejected`: validation/profile preparation refused; no queue submission.
- `enqueued`: the PTY queue accepted one byte batch; not proof of delivery or agent acceptance.
- `unknown_delivery`: a durable intent exists without reliable resolution, or queue submission failed.

An operation ID permanently binds one payload. Every later submit of that same
payload returns its stored receipt or unresolved intent without validation or
input. Another payload with the same ID is refused. Partial/corrupt journals and
intent claims without valid data produce `interaction_journal_error` and must
never be retried as a new dispatch. Missing receipt writes after input leave the
prewritten intent. A client timeout must be reconciled using the original ID;
creating another operation can duplicate the effect and is not recovery.

Private journals live in the server session data directory under
`interaction-operations-v1`, with directory entries and the prewrite intent
synced before any send. This durability implementation is enabled on Unix only;
other platforms reject dispatch until equivalent durable directory semantics are
implemented. No automatic journal expiry/deletion is provided because removing
intents would permit retries to redispatch.

The guard is **not atomic with external terminal writers or the agent**. PTY
output can change asynchronously after observation, other clients/humans/raw
write methods can interleave, and queue acceptance does not prove which dialog
the agent consumed. Do not describe this as a transaction with agent intent.

## Native profile evidence still required

Monster's installed Codex is 0.162.0 and Claude Code is 2.1.284. The matching
official Codex source tag `rust-v0.162.0` resolves to
`1f3f93473394b620b35580859b7e6864f7a9f948`.
`codex-rs/tui/src/bottom_pane/request_user_input/mod.rs` provides a feasible narrow
profile: single-question options, freeform composer and notes/custom entry.
Source proves digit shortcuts immediately commit/advance, including the
`None of the above` digit; that digit alone does not enter custom text.
Navigating to that row and Enter/Tab enters notes. Submit bindings can be
remapped, so any recognizer must require the exact supported footer.

Before enabling this profile, capture owned real Codex detection buffers for
choice/freeform/custom entry, verify native parsing against those buffers, verify
exact input batching/paste boundaries against the running owned TUI, persist and
check custom parent lineage, and confirm the agent actually receives the custom
answer. Repository/source fixtures alone do not satisfy this evidence. Claude
and all unverified shapes (multi-question, remapped, truncated menus, existing
drafts, confirmation overlays) remain unsupported.
