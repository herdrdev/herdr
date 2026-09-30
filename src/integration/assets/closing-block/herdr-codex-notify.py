#!/usr/bin/env python3
# HERDR_INTEGRATION_VERSION=4
"""Codex `notify` handler -> herdr turn-end status.

Codex invokes the notify program with a single JSON argument. For a finished
turn it carries the assistant's final message, which is all we need:

    notify = ["python3", "/path/to/herdr-codex-notify.py"]

Codex already has a notify entry on this machine. Chain rather than replace:

    notify = ["/path/to/chain.sh"]     # calls the existing handler, then this

Fails silent and non-blocking in every path.

Replay protection retains the latest 64 documented (thread-id, turn-id) pairs.
It rejects proven duplicate deliveries, but cannot order a first-seen late UUID;
if the ledger cannot be persisted, reporting falls back to the legacy best effort.
"""

from __future__ import annotations

import json
import fcntl
import os
import sys
import tempfile
from datetime import datetime, timezone

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from closing_block import parse, settle_ready  # noqa: E402
from herdr_status import accepts_payload, mirror_path, report, reserve_sequence  # noqa: E402

# Codex has used both spellings across versions.
TURN_DONE = {"agent-turn-complete", "agent_turn_complete", "turn-ended", "turn_ended"}
MESSAGE_KEYS = ("last-assistant-message", "last_assistant_message", "message")
REPORTED_TURN_LIMIT = 64


def reported_turns_path(pane_id: str) -> str:
    return f"{mirror_path(pane_id)}.codex-turns"


def _reported_turn_key(session_id: str, turn_id: str) -> list[str]:
    return [session_id, turn_id]


def _read_reported_turns(path: str) -> list[list[str]]:
    try:
        with open(path, encoding="utf-8") as fh:
            values = json.load(fh)
    except (OSError, ValueError):
        return []
    if not isinstance(values, list):
        return []
    return [
        value
        for value in values
        if (
            isinstance(value, list)
            and len(value) == 2
            and all(isinstance(part, str) and part for part in value)
        )
    ][-REPORTED_TURN_LIMIT:]


def report_unreported_turn(pane_id: str, session_id: str, turn_id: str, deliver):
    """Deliver once, persisting the replay key only after socket success."""
    if not session_id or not turn_id:
        return deliver()
    path = reported_turns_path(pane_id)
    try:
        lock = open(f"{path}.lock", "a", encoding="utf-8")
    except OSError:
        # The adapter was historically stateless; an unwritable optional
        # replay ledger must not suppress all turn-end reports.
        return deliver()
    with lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        values = _read_reported_turns(path)
        key = _reported_turn_key(session_id, turn_id)
        if key in values:
            return None
        outcome = deliver()
        if outcome.get("socket") is True:
            values = [*values, key][-REPORTED_TURN_LIMIT:]
            try:
                fd, tmp = tempfile.mkstemp(dir=os.path.dirname(path), suffix=".tmp")
                with os.fdopen(fd, "w", encoding="utf-8") as fh:
                    json.dump(values, fh)
                os.replace(tmp, path)
            except OSError:
                pass
        return outcome


def load_payload(argv: list[str]) -> dict:
    for arg in argv:
        try:
            body = json.loads(arg)
        except ValueError:
            continue
        if isinstance(body, dict):
            return body
    if not sys.stdin.isatty():
        try:
            body = json.load(sys.stdin)
            if isinstance(body, dict):
                return body
        except (ValueError, OSError):
            pass
    return {}


def title_from(payload: dict, pane_id: str) -> str | None:
    """A stable session name for the pane, since codex ships none.

    Codex's own resume picker previews the first user message, so use the same
    thing -- but only the *first* one. `input-messages` is per turn, so
    re-deriving every turn would rename the pane on every prompt. The first
    title wins and is kept in the mirror for the rest of the session.
    """
    try:
        with open(mirror_path(pane_id), encoding="utf-8") as fh:
            existing = json.load(fh).get("title")
        if isinstance(existing, str) and existing:
            return existing
    except (OSError, ValueError, AttributeError):
        pass

    messages = payload.get("input-messages") or payload.get("input_messages") or []
    if not isinstance(messages, list):
        return None
    # Last message, not first: the injected AGENTS.md / permissions preamble is
    # prepended, so the human's actual prompt is at the end.
    for msg in reversed(messages):
        text = msg if isinstance(msg, str) else ""
        for line in text.splitlines():
            line = line.strip().lstrip("#").strip()
            if not line or line.startswith("<") or line.lower().startswith("agents.md"):
                continue
            return line[:80]
    return None


def main() -> int:
    if os.environ.get("HERDR_ENV") != "1" or not os.environ.get("HERDR_PANE_ID"):
        return 0
    # This orders distinct notification processes by invocation. Turn UUIDs
    # identify a turn but are not assumed to encode chronology.
    seq = reserve_sequence()
    payload = load_payload(sys.argv[1:])
    if not accepts_payload(payload):
        return 0
    kind = str(payload.get("type") or payload.get("event") or "")
    if kind and kind not in TURN_DONE:
        return 0

    pane_id = os.environ["HERDR_PANE_ID"]
    session_id = payload.get("thread-id") or payload.get("thread_id")
    turn_id = payload.get("turn-id") or payload.get("turn_id")
    session_id = session_id if isinstance(session_id, str) else ""
    turn_id = turn_id if isinstance(turn_id, str) else ""
    text = next(
        (payload[k] for k in MESSAGE_KEYS if isinstance(payload.get(k), str)), None
    )
    if not text:
        return 0
    # Missing task evidence is reported explicitly so an abbreviated reply does
    # not clear unresolved decisions from an earlier authoritative report.
    block = parse(text)
    outcome = report_unreported_turn(
        pane_id,
        session_id,
        turn_id,
        lambda: report(
            agent="codex",
            blocking=block.blocking,
            agents=block.agents_running,
            gates=block.wire_gates(),
            items=block.wire_items(),
            decisions=block.wire_decisions(),
            agent_names=block.agents,
            completion=block.completion,
            external_wait=block.external_wait,
            parse_status=block.parse_status,
            workers_unknown=block.workers_unknown,
            settle_ready=settle_ready(text),
            session_id=session_id or None,
            title=title_from(payload, pane_id),
            seq=seq,
            last_turn_at=datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
        ),
    )
    if outcome is not None and os.environ.get("HERDR_CLOSING_BLOCK_DEBUG"):
        print(json.dumps(outcome["payload"]), file=sys.stderr)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except SystemExit:
        raise
    except Exception:  # noqa: BLE001 -- never wedge a turn
        raise SystemExit(0)
