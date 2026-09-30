#!/usr/bin/env python3
# HERDR_INTEGRATION_VERSION=4
"""Claude Code `Stop` hook -> herdr turn-end status.

Installed *beside* herdr's managed `herdr-agent-state.sh`, which herdr overwrites
on integration update and which today reports session identity only.

    "Stop": [{"hooks": [{"type": "command",
      "command": "python3 /path/to/herdr-closing-block.py"}]}]

The hook is the trigger -- it fires deterministically at turn end, unlike a tool
call the model has to remember. It extracts the closing block from the last
main-chain assistant message and hands the structured counts to herdr_status.

Fails silent and non-blocking in every path.
"""

from __future__ import annotations

import json
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from closing_block import parse, settle_ready  # noqa: E402
from herdr_status import report, reserve_sequence  # noqa: E402

# Claude Code fires Stop hooks concurrently with flushing the final assistant
# message to the transcript. Reading immediately can find no assistant text at
# all (first turn) or only the previous turn's text -- both silently corrupt the
# report. Poll until the newest main-chain assistant text sits after the newest
# user row, i.e. the reply this Stop belongs to has landed.
FLUSH_WAIT_SECONDS = 3.0
FLUSH_POLL_INTERVAL = 0.15


def _read_rows(transcript_path: str) -> list[dict]:
    rows: list[dict] = []
    try:
        with open(transcript_path, encoding="utf-8") as fh:
            for line in fh:
                if not line.strip():
                    continue
                try:
                    rows.append(json.loads(line))
                except ValueError:
                    # A torn trailing line mid-flush must not discard the file.
                    continue
    except OSError:
        return []
    return rows


def _scan(rows: list[dict]) -> tuple[str | None, bool]:
    """Newest main-chain assistant text, and whether it postdates user input."""
    last_user_idx = -1
    for idx, row in enumerate(rows):
        if row.get("type") == "user" and not row.get("isSidechain"):
            last_user_idx = idx
    for idx in range(len(rows) - 1, -1, -1):
        row = rows[idx]
        if row.get("type") != "assistant" or row.get("isSidechain"):
            continue
        text = "\n".join(
            c.get("text", "")
            for c in row.get("message", {}).get("content", [])
            if c.get("type") == "text"
        )
        if text.strip():
            return text, idx > last_user_idx
    return None, False


def last_assistant_text(transcript_path: str) -> str | None:
    deadline = time.monotonic() + FLUSH_WAIT_SECONDS
    while True:
        text, fresh = _scan(_read_rows(transcript_path))
        if text is not None and fresh:
            return text
        if time.monotonic() >= deadline:
            # Fall back to whatever is readable: stale text still beats a
            # silently dropped report, and None keeps the old skip behavior.
            return text
        time.sleep(FLUSH_POLL_INTERVAL)


def main() -> int:
    if os.environ.get("HERDR_ENV") != "1" or not os.environ.get("HERDR_PANE_ID"):
        return 0
    try:
        payload = json.load(sys.stdin)
    except (ValueError, OSError):
        return 0
    if payload.get("agent_id"):  # subagent stop -- never speaks for the pane
        return 0

    # Reserve the ordering at hook invocation, before transcript polling. An
    # older Stop may read its final row and then stall while a newer Stop reads
    # and reports; allocating its sequence after that stall would revive the
    # old turn.
    seq = reserve_sequence()
    text = last_assistant_text(payload.get("transcript_path") or "")
    if text is None:
        return 0
    # A short reply without task evidence still emits a sequenced report, but
    # marks that evidence missing so the runtime can preserve unresolved state.
    block = parse(text)

    outcome = report(
        agent="claude",
        blocking=block.blocking,
        agents=block.agents_running,
        gates=block.wire_gates(),
        items=block.wire_items(),
        decisions=block.wire_decisions(),
        agent_names=block.agents,
        contract=block.contract,
        contract_met=block.contract_met,
        completion=block.completion,
        external_wait=block.external_wait,
        parse_status=block.parse_status,
        workers_unknown=block.workers_unknown,
        settle_ready=settle_ready(text),
        session_id=payload.get("session_id"),
        session_path=payload.get("transcript_path"),
        seq=seq,
    )
    if os.environ.get("HERDR_CLOSING_BLOCK_DEBUG"):
        print(json.dumps(outcome["payload"]), file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
