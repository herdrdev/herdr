# Guarded agent interactions (experimental prerequisite)

The JSON runtime API adds `agent.interaction.get`, `agent.interaction.submit`,
and `agent.interaction.receipt`. Default configuration enables no native
interaction profile. Codex and unverified agent/version/layout combinations
remain unsupported. Raw `agent.send_keys` and `pane.send_text` retain their
existing guarantees.

The opt-in `HERDR_GUARDED_CLAUDE_PROFILE=2.1.284-custom-v1-experimental` token
is scoped to the owned Claude Code 2.1.284/default-style captures described below.
Old `2.1.284-experimental` and `2.1.284-numeric-v1-experimental` tokens are disabled.
This is an experimental profile, not a claim of general production readiness.

## Observation and payload binding

`get` accepts `{ "target": "<terminal id or pane target>" }`. Result type
`agent_interaction` contains `observation`, `supported`, `unsupported_reason`,
and an optional typed `dialog` with profile, phase, question, option identities,
selected option and supported actions.

```json
{
  "terminal_id": "<exact terminal id>",
  "server_instance_id": "<native server process incarnation>",
  "runtime_pid": 123,
  "agent": "claude",
  "agent_session": {"source":"<source>","agent":"claude","kind":"id","value":"<session id>"},
  "state_change_seq": 42,
  "content_digest": "<sha256 of complete UTF-8 detection text>",
  "style_digest": "<sha256 of paired bottom-buffer ANSI>"
}
```

Text and ANSI are collected under the same terminal-core mutex from the bottom
buffer, independent of viewport scrolling. `content_digest` retains its text-only
meaning. The additive `style_digest` field is optional for schema compatibility;
this enabled profile requires it to equal the current style digest. Missing
runtime/agent/session identity or snapshot access returns an error. Server
incarnation and runtime PID invalidate observations across server or PTY changes.

`submit` requires `operation_id`, `payload_digest`, the exact `expected`
observation and one typed `action`:

```json
{"type":"choose","option_id":"<native option id>"}
{"type":"free_text","text":"<answer>"}
{"type":"begin_custom","option_id":"choice-4"}
{"type":"submit_custom","text":"<answer>","parent_operation_id":"<begin operation>"}
```

The enum is shared; a profile admits only its explicitly supported actions.
Unknown fields and raw keys are rejected. Operation IDs contain 1..128 ASCII
letters, digits, underscores or hyphens. `payload_digest` is lowercase SHA-256
of compact UTF-8 serde JSON `[operation_id, expected, action]`. Preserve the
Rust declaration order shown above; optional `style_digest`, when present,
follows `content_digest`. An absent optional field is omitted. Action order
is `type`, then `option_id`, or `text`, or `text` then `parent_operation_id`.
Do not normalize answer strings.

## Claude custom-entry contract

The profile requires a blocked Claude agent and exact 2.1.284 banner, one checkbox
question header, contiguous rows 1..4, one selected row, three suggestions,
`Type something.` as custom row 4, lower rule, `5. Chat about this`, and exact
captured footer. Wrapped, remapped, multi-question or unknown layouts reject.

Initial phase `choose` supports only `begin_custom`. The empty, unfocused
placeholder must have the captured inactive RGB 153/153/153 foreground. This
styles check rejects a nonempty draft literally equal to `Type something.`,
which is indistinguishable in plain text alone. Compilation produces exactly
one numeric 4 key event through the runtime keyboard encoder, without Enter,
arrows, waiting or retries.

Phase `custom_entry` supports only `submit_custom`. It requires selected row 4,
the captured `ctrl+g to edit in nano` footer, inverse first placeholder character
and dim remaining placeholder characters. Filled, missing, indexed/altered or
unknown placeholder styles reject. The current row is checked; an older empty
row cannot supply style authority. These exact styles are version/theme scoped;
other themes remain unsupported.

Submission requires a successful `enqueued` parent receipt and its complete
validated BeginCustom 4 request/dialog/input digest. Parent and child must have
the same terminal, native server incarnation, runtime PID, agent/session,
profile, question/options and exact blocked state sequence. The child expected
text/style digests must match the current locked snapshot. Unknown, rejected,
legacy, corrupt, wrong-owner, earlier-episode or changed-dialog parents reject.
A server restart requires a fresh parent; there is no cross-incarnation shortcut.

Custom text must be nonempty after trimming, at most 4096 UTF-8 bytes, and contain
no control characters, including escape, newline, carriage return and DEL.
The original text is preserved. Runtime bracketed paste must be enabled.
Compilation uses native bracketed paste followed by one runtime-encoded Enter
in one queue submission. Choose and FreeText remain unsupported. Queue acceptance
is not answer acceptance; actual question resolution and literal transcript
answer still require owned live verification for each candidate gate.

## Durable receipts and one-child recovery

`receipt` accepts `{ "operation_id": "<original operation id>" }`. Submit and
receipt return `agent_interaction_receipt` with operation ID, payload digest,
outcome and optional code:

- `rejected`: validation refused before queue submission.
- `enqueued`: one batch accepted by the PTY queue; no delivery/agent acceptance assertion.
- `unknown_delivery`: durable intent without reliable resolution; never redispatch.

An operation ID permanently binds one payload. Later identical submits return
its stored receipt/intent without preparation or input; another payload with
that ID is refused. A timeout must be reconciled with the original ID. Creating
another operation ID is not recovery.

Private Unix journals live under session data `interaction-operations-v1`.
Exclusive directories/records are synced in this order: intent, complete
`request.json` before preparation, then recognized request/dialog/input digest
in `validated.json` before any queue submission. A custom child also exclusively
claims `custom-child.json` in the parent's directory, containing the complete
child request and synced before the child validated record and queue. Each
parent can authorize at most one child. Partial claims consume the parent;
unknown children and different child IDs cannot replay its input. Legacy intents
cannot be retrofitted with validated context. Old journals are retained.

Filesystem access uses descriptor-relative nofollow opens. Ancestors must belong
to the current user or root and prohibit group/other writes, apart from root-owned
sticky temporary directories. Journal/operation directories require current-user
ownership and 0700; regular records require 0600 and one link. Existing unsafe
objects reject without chmod adoption. Symlinks, hardlinks and special files
reject. Other platforms remain unsupported until equivalent durable semantics
exist. No automatic expiry/deletion is provided because removing intents could
permit dispatch again.

## Evidence and limits

The old combined CSI-arrow/Enter batch was enqueued in an owned fixture but
Claude reported that the user declined the question. That operation was never
resent. Installed Select source shows empty custom input submission can call
cancel; this is consistent with the decline, but prior key consumption is not
proven.

The separate numeric 4 repair at `efd6a6181c61cb305361a13f6b5b8f8ec29d0bcb`
passed independent native tests and one fresh owned live focus-only gate. A real
custom row and editor footer were observed with the same Claude session; no text
was submitted at that precursor. Native state sequence remained 3 while content
changed, motivating exact sequence binding and paired style observation.

Private installed Claude ELF evidence was read from version 2.1.284, SHA-256
`5cd90aabd83f8a15136c35aa37bb1d92b348993573316643dc3fe4e04afbf88f`:
Select handler near 221679130 focuses an empty input on its numeric index;
Epe near 221672585 renders placeholder/draft differently in styles;
AskUserQuestion row near 226956446 defines its custom input; TextInput near
214831325 and paste handler MWe defer Return while bracketed paste is pending.
No proprietary source excerpt or live transcript is included in this repository.
These source facts support compilation design; they are not live answer proof.

Banner recognition is not native process-binary attestation. Native session
metadata is recorded identity, and an earlier owned restore exposed stale
session metadata; this patch does not fix that restore behavior. Enrollment
therefore remains scoped to fresh owned fixtures with independently checked
process/version/session lineage and no prior input. Generic draft safety and
production readiness must not be inferred from the earlier text-only profile.
`--source detection --format ansi` currently returns plain detection text in the
existing read API; it cannot supply style proof. This handler's paired snapshot
is a distinct internal path; real styled evidence used recent-unwrapped ANSI.

The handler serializes native validation/journaling/queue submission, but PTY
output and other terminal writers remain independent. They can change input
state after the checked snapshot. No transaction with external agent intent,
exact physical delivery, or exclusion of human/raw writers is claimed.
