#!/usr/bin/env python3
# HERDR_INTEGRATION_VERSION=6
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
import hashlib
import os
import sys
import time
from datetime import datetime, timezone

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from closing_block import parse, settle_ready  # noqa: E402
from herdr_status import report, reserve_sequence  # noqa: E402

# Claude Code fires Stop hooks concurrently with flushing the final assistant
# message to the transcript: at hook invocation the transcript usually still
# ends at the previous tool result or user row, and under load the row can take
# longer than any sane hook budget to land. Claude Code therefore hands the
# final reply to the hook as `last_assistant_message`, which is used directly
# with no transcript read. Only a payload without it (an older Claude Code, or
# a turn that ended without text) falls back to polling the transcript until
# the newest main-chain assistant text sits after the newest user row. The cap
# bounds that fallback; at the old 3.0 s it held the prompt on ~29% of real
# Stops (stop_hook_summary durations, 7 days, ub1).
FLUSH_WAIT_SECONDS = 1.0
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


def _scan_turn(rows: list[dict]) -> tuple[str | None, bool, str | None]:
    """Newest main-chain assistant text, freshness, and transcript timestamp."""
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
            timestamp = row.get("timestamp")
            return text, idx > last_user_idx, timestamp if isinstance(timestamp, str) else None
    return None, False, None


def _scan(rows: list[dict]) -> tuple[str | None, bool]:
    text, fresh, _timestamp = _scan_turn(rows)
    return text, fresh


def last_assistant_text(transcript_path: str) -> str | None:
    return last_assistant_turn(transcript_path)[0]


def last_assistant_turn(transcript_path: str) -> tuple[str | None, str | None]:
    if not transcript_path or not os.path.isfile(transcript_path):
        # Nothing to wait for: a missing transcript will not appear mid-Stop.
        return None, None
    deadline = time.monotonic() + FLUSH_WAIT_SECONDS
    while True:
        text, fresh, timestamp = _scan_turn(_read_rows(transcript_path))
        if text is not None and fresh:
            return text, timestamp
        if time.monotonic() >= deadline:
            # Fall back to whatever is readable: stale text still beats a
            # silently dropped report, and None keeps the old skip behavior.
            return text, timestamp
        time.sleep(FLUSH_POLL_INTERVAL)


def stop_turn(payload: dict) -> tuple[str | None, str | None]:
    """Final reply text for this Stop and its transcript timestamp, if known.

    The payload's `last_assistant_message` is the reply this Stop belongs to, so
    it needs no flush wait; its timestamp falls back to invocation time.
    """
    text = payload.get("last_assistant_message")
    if isinstance(text, str) and text.strip():
        return text, None
    return last_assistant_turn(payload.get("transcript_path") or "")


def lint_result_blocks(session_id: str | None, last_turn_at: str | None, invoked_at: str) -> bool:
    """Whether cap-lint has a newer invalid result for this assistant turn."""
    if not session_id:
        return False
    root = os.environ.get("HERDR_CLOSING_BLOCK_LINT_RESULT_DIR")
    if root is None:
        configured = os.environ.get("CAP_LINT_STATE_DIR")
        if configured:
            marker_dir = configured
        else:
            state = os.environ.get("XDG_STATE_HOME") or os.path.expanduser("~/.local/state")
            marker_dir = os.path.join(state, "scalable-agent-fleet", "cap-lint")
        root = os.path.join(os.path.dirname(marker_dir), os.path.basename(marker_dir) + "-closing")
    path = os.path.join(root, hashlib.sha256(session_id.encode()).hexdigest() + ".json")
    try:
        stat = os.stat(path)
        with open(path, encoding="utf-8") as handle:
            result = json.load(handle)
        turn_time = datetime.fromisoformat((last_turn_at or invoked_at).replace("Z", "+00:00")).timestamp()
        return stat.st_mtime > turn_time and isinstance(result, dict) and result.get("valid") is False
    except (OSError, ValueError, TypeError):
        return False


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
    text, last_turn_at = stop_turn(payload)
    if text is None:
        return 0
    invocation_time = datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")
    if last_turn_at is None:
        _transcript_text, _fresh, transcript_timestamp = _scan_turn(
            _read_rows(payload.get("transcript_path") or "")
        )
        last_turn_at = transcript_timestamp
    last_turn_at = last_turn_at or invocation_time
    if lint_result_blocks(payload.get("session_id"), last_turn_at, invocation_time):
        if os.environ.get("HERDR_CLOSING_BLOCK_DEBUG"):
            print("closing-block: skipped report after newer invalid cap-lint result", file=sys.stderr)
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
        state=block.herdr_state,
        settle_ready=(block.completion == "complete" if block.source == "record" else settle_ready(text)),
        closing_source=block.source,
        parse_message=(block.message() if block.record_errors else None),
        session_id=payload.get("session_id"),
        session_path=payload.get("transcript_path"),
        seq=seq,
        last_turn_at=last_turn_at,
    )
    if os.environ.get("HERDR_CLOSING_BLOCK_DEBUG"):
        print(json.dumps(outcome["payload"]), file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
