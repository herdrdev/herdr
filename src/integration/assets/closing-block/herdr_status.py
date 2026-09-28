"""Agent-agnostic v2 turn-end status contract for herdr.

The closing-block adapter writes one payload:

    {"v": 2, "agent": "claude", "blocking": 1, "agents": 0,
     "completion": "incomplete", "external_wait": null,
     "parse_status": "ok", "workers_unknown": false,
     "gates": [{"n": 1, "label": "Gate", "text": "...", "blocking": true,
                "pr": null,
                "ticket": null, "url": null, "default": null,
                "default_at": null}],
     "items": [], "decisions": [], "agent_names": []}

The arrays are sent through the existing agent-report channel. The blocked state
label names the action-point kind while the full item stays in its payload array.
A turn-end hook never raises.
"""

from __future__ import annotations

import fcntl
import json
import os
import random
import socket
import tempfile
import time
from datetime import datetime, timezone
from typing import Any

from closing_block import GATE_LABELS, HUMAN_INPUT_LABELS

# HERDR_INTEGRATION_VERSION=3
VERSION = 2


STATES = ("idle", "working", "blocked", "unknown")
_HUMAN_INPUT_LABELS = {label.casefold() for label in HUMAN_INPUT_LABELS}
_GATE_LABELS = {label.casefold() for label in GATE_LABELS}


def state_for(
    blocking: int,
    agents: int,
    action_points: int = 0,
    external_wait: str | None = None,
) -> str:
    if blocking > 0 or action_points > 0:
        return "blocked"
    if agents > 0 or external_wait:
        return "working"
    return "idle"


def resolve_state(
    blocking: int,
    agents: int,
    override: str | None,
    action_points: int = 0,
    external_wait: str | None = None,
) -> str:
    """Counts imply the state unless a caller names one it knows better.

    A mid-turn source knows the turn is still running, which zero counts alone
    would read as `idle` and publish as a finished turn. An unknown override is
    ignored rather than trusted, because a junk state string would otherwise be
    pushed to the server verbatim.
    """
    if isinstance(override, str) and override in STATES:
        return override
    return state_for(blocking, agents, action_points, external_wait)


def _item_text(item: dict[str, Any]) -> str:
    return str(item.get("text") or "").strip()


def _normalize_item(
    value: dict[str, Any] | str,
    *,
    index: int,
    label: str,
) -> dict[str, Any]:
    if isinstance(value, str):
        text = value
        return {
            "n": index,
            "label": label,
            "text": text,
            "blocking": True,
            "pr": None,
            "ticket": None,
            "url": None,
            "default": None,
            "default_at": None,
        }
    item = dict(value)
    item.setdefault("n", index)
    item.setdefault("label", label)
    item.setdefault("text", "")
    item.setdefault("blocking", True)
    item.setdefault("pr", None)
    item.setdefault("ticket", None)
    item.setdefault("url", None)
    item["default"] = None
    item["default_at"] = None
    return item


def _normalize_decision(
    value: dict[str, Any],
    *,
    index: int,
) -> dict[str, Any]:
    decision = dict(value)
    decision.setdefault("n", index)
    decision.setdefault("text", "")
    decision.setdefault("recommendation", decision["text"])
    decision["reversible"] = True
    if decision.get("decided_at") is None:
        decision["decided_at"] = datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")
    return decision


def blocked_state_label(
    blocking: int,
    action_points: list[dict[str, Any]] | None = None,
) -> str:
    """Name the blocked action kind, never the action text.

    Row width is scarce and the pane's own title already sits beside it, so a
    truncated body crowds out the identity that says which agent is asking.
    Gate text stays in `closing_gates` and `gates[]`; other action text stays
    in `items[]`.
    """
    action_points = action_points or []
    if len(action_points) == blocking == 1:
        return str(action_points[0].get("label") or "action point").lower()
    if action_points and len(action_points) == blocking:
        if all(
            str(item.get("label") or "").casefold() in _GATE_LABELS
            for item in action_points
        ):
            return f"{blocking} gates"
        return f"{blocking} action points"
    if blocking <= 0:
        return "blocked"
    if action_points:
        return f"{blocking} action points"
    return "gate" if blocking == 1 else f"{blocking} gates"


def message_for(
    blocking: int,
    agents: int,
    gates: list[dict[str, Any]],
    action_points: list[dict[str, Any]] | None = None,
) -> str | None:
    if blocking > 0:
        pending = [*gates, *(action_points or [])]
        head = _item_text(pending[0]) if pending else ""
        extra = f" (+{blocking - 1})" if blocking > 1 else ""
        return ((head or f"{blocking} blocking")[:80]) + extra
    if agents > 0:
        return f"{agents} agent{'s' if agents != 1 else ''} running"
    if action_points:
        return _item_text(action_points[0])[:80] or None
    return None


def mirror_path(pane_id: str) -> str:
    root = os.environ.get("XDG_STATE_HOME") or os.path.expanduser("~/.local/state")
    d = os.path.join(root, "herdr", "agent-status")
    os.makedirs(d, exist_ok=True)
    return os.path.join(d, f"{pane_id.replace(':', '_')}.json")


def _blocking_item_identity(item: dict[str, Any]) -> tuple:
    return (
        str(item.get("label") or "").strip().lower(),
        str(item.get("text") or "").strip(),
        item.get("pr"),
        item.get("ticket"),
        item.get("url"),
    )


def _merge_blocking_items(prior: Any, current: Any) -> list[dict[str, Any]]:
    merged = [item for item in prior or [] if isinstance(item, dict)]
    identities = {_blocking_item_identity(item) for item in merged}
    for item in current or []:
        if not isinstance(item, dict):
            continue
        if str(item.get("label") or "").strip().lower() not in {
            "gate",
            "answer",
            "verify",
        }:
            continue
        identity = _blocking_item_identity(item)
        if identity not in identities:
            merged.append(item)
            identities.add(identity)
    return merged


def write_mirror(pane_id: str, payload: dict) -> str | None:
    """Atomic write -- a torn status file is worse than a stale one."""
    path = mirror_path(pane_id)
    lock_path = f"{path}.lock"
    # The server drops reports whose seq is not strictly newer; the mirror must
    # apply the same ordering or a stale writer leaves it disagreeing with the
    # server indefinitely.
    try:
        with open(lock_path, "a", encoding="utf-8") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            try:
                with open(path, encoding="utf-8") as fh:
                    prior = json.load(fh)
                prior_seq = prior.get("seq") if isinstance(prior, dict) else None
            except (OSError, ValueError):
                prior = {}
                prior_seq = None
            if (
                isinstance(prior_seq, int)
                and isinstance(payload.get("seq"), int)
                and payload["seq"] <= prior_seq
            ):
                return None
            mirror_payload = dict(payload)
            same_session = (
                isinstance(prior, dict)
                and prior.get("session_id") == payload.get("session_id")
            )
            parse_status = payload.get("parse_status")
            if same_session and parse_status == "missing":
                for key in (
                    "blocking",
                    "agents",
                    "gates",
                    "items",
                    "decisions",
                    "agent_names",
                    "external_wait",
                    "workers_unknown",
                ):
                    if key in prior:
                        mirror_payload[key] = prior[key]
            elif same_session and parse_status == "malformed":
                for key in (
                    "agents",
                    "decisions",
                    "agent_names",
                    "external_wait",
                    "workers_unknown",
                ):
                    if key in prior:
                        mirror_payload[key] = prior[key]
                merged_gates = _merge_blocking_items(
                    prior.get("gates"), payload.get("gates")
                )
                merged_items = _merge_blocking_items(
                    prior.get("items"), payload.get("items")
                )
                if merged_gates or "gates" in prior or "gates" in payload:
                    mirror_payload["gates"] = merged_gates
                if merged_items or "items" in prior or "items" in payload:
                    mirror_payload["items"] = merged_items
                recognized_blocking = len(merged_gates) + sum(
                    str(item.get("label") or "").strip().lower()
                    in {"answer", "verify"}
                    for item in merged_items
                )
                mirror_payload["blocking"] = max(
                    int(prior.get("blocking") or 0),
                    int(payload.get("blocking") or 0),
                    recognized_blocking,
                )
            fd, tmp = tempfile.mkstemp(dir=os.path.dirname(path), suffix=".tmp")
            with os.fdopen(fd, "w", encoding="utf-8") as fh:
                json.dump(mirror_payload, fh)
            os.replace(tmp, path)
            return path
    except OSError:
        return None


def _rpc(sock_path: str, source: str, method: str, params: dict) -> bool:
    req = {
        "id": f"{source}:{int(time.time() * 1000)}:{random.randrange(10**6):06d}",
        "method": method,
        "params": params,
    }
    client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    client.settimeout(0.5)
    try:
        client.connect(sock_path)
        client.sendall((json.dumps(req) + "\n").encode())
        raw = client.recv(4096)
        if not raw:
            return False
        response = json.loads(raw.splitlines()[0])
        return (
            isinstance(response, dict)
            and response.get("id") == req["id"]
            and "result" in response
            and "error" not in response
        )
    except (OSError, ValueError, json.JSONDecodeError):
        return False
    finally:
        client.close()


def accepts_payload(payload: object) -> bool:
    """Return false for an old/new wire version without raising or reporting."""
    if not isinstance(payload, dict):
        return False
    version = payload.get("v")
    return version is None or version == VERSION


def reserve_sequence() -> int:
    """Reserve report ordering before a caller performs a fallible slow read."""
    return time.time_ns()


def report(
    *,
    agent: str,
    blocking: int,
    agents: int,
    wait: str | None = None,
    eta_s: int | None = None,
    gates: list[dict[str, Any] | str] | None = None,
    items: list[dict[str, Any] | str] | None = None,
    decisions: list[dict[str, Any]] | None = None,
    agent_names: list[str] | None = None,
    contract: str | None = None,
    contract_met: bool | None = None,
    completion: str = "missing",
    external_wait: str | None = None,
    parse_status: str = "ok",
    workers_unknown: bool = False,
    session_id: str | None = None,
    session_path: str | None = None,
    title: str | None = None,
    pane_id: str | None = None,
    sock_path: str | None = None,
    state: str | None = None,
    seq: int | None = None,
) -> dict:
    """Push one v2 turn-end status. Never raises; returns what it did."""
    gate_objects = [
        _normalize_item(value, index=index, label="Gate")
        for index, value in enumerate(gates or [], start=1)
    ]
    item_objects = [
        _normalize_item(value, index=index, label="Answer")
        for index, value in enumerate(items or [], start=1)
    ]
    all_items = [*gate_objects, *item_objects]
    for item in all_items:
        if str(item.get("label") or "").casefold() in _HUMAN_INPUT_LABELS:
            item["blocking"] = True
    action_points = [
        item
        for item in item_objects
        if (
            str(item.get("label") or "").casefold() in _HUMAN_INPUT_LABELS
            and str(item.get("label") or "").casefold() not in _GATE_LABELS
        )
    ]
    blocking = max(blocking, len(gate_objects) + len(action_points))
    decision_objects = [
        _normalize_decision(value, index=index)
        for index, value in enumerate(decisions or [], start=1)
    ]
    agent_names = agent_names or []
    pane_id = pane_id or os.environ.get("HERDR_PANE_ID") or ""
    sock_path = sock_path or os.environ.get("HERDR_SOCKET_PATH") or ""

    seq = seq if isinstance(seq, int) else reserve_sequence()
    reported_at = datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")
    completion = (
        completion if completion in {"complete", "incomplete", "missing"} else "missing"
    )
    parse_status = (
        parse_status if parse_status in {"ok", "missing", "malformed"} else "malformed"
    )
    external_wait = external_wait.strip() if isinstance(external_wait, str) else None
    external_wait = external_wait or None
    workers_unknown = workers_unknown is True
    dependencies_authoritative = parse_status == "ok"
    reported_agents = agents if dependencies_authoritative and not workers_unknown else None
    requested_state = state
    state = resolve_state(
        blocking, agents, state, len(action_points), external_wait
    )
    if not dependencies_authoritative and requested_state != "working":
        state = "unknown"
    payload = {
        "v": VERSION,
        "agent": agent,
        "seq": seq,
        "reported_at": reported_at,
        "state": state,
        "blocking": blocking,
        "completion": completion,
        "external_wait": external_wait,
        "parse_status": parse_status,
        "workers_unknown": workers_unknown,
    }
    if dependencies_authoritative:
        payload["gates"] = gate_objects
        payload["items"] = item_objects
        payload["decisions"] = decision_objects
        payload["agent_names"] = agent_names
    elif parse_status == "malformed":
        if gate_objects:
            payload["gates"] = gate_objects
        if action_points:
            payload["items"] = action_points
    if reported_agents is not None:
        payload["agents"] = reported_agents
    if title:
        payload["title"] = title
    if state == "working" and wait and isinstance(eta_s, int) and eta_s >= 0:
        payload["wait"] = wait
        payload["eta_s"] = eta_s

    outcome = {"payload": payload, "mirror": None, "socket": False}
    if pane_id:
        mirror_payload = dict(payload)
        if session_id:
            mirror_payload["session_id"] = session_id
        outcome["mirror"] = write_mirror(pane_id, mirror_payload)
    if not (pane_id and sock_path):
        return outcome

    source = f"herdr:{agent}-closing-block"
    session_params = {
        "pane_id": pane_id,
        "source": source,
        "agent": agent,
        "seq": seq - 1,
    }
    if session_id:
        session_params["agent_session_id"] = session_id
    if session_path:
        session_params["agent_session_path"] = session_path

    agent_params = {"pane_id": pane_id, "source": source, "agent": agent,
                    "state": state, "seq": seq, "v": VERSION,
                    "reported_at": reported_at,
                    "completion": completion,
                    "external_wait": external_wait,
                    "parse_status": parse_status,
                    "workers_unknown": workers_unknown}
    if dependencies_authoritative:
        agent_params["gates"] = gate_objects
        agent_params["items"] = item_objects
        agent_params["decisions"] = decision_objects
    elif parse_status == "malformed":
        if gate_objects:
            agent_params["gates"] = gate_objects
        if action_points:
            agent_params["items"] = action_points
    if reported_agents is not None:
        agent_params["agents"] = reported_agents
    if state == "working" and wait and isinstance(eta_s, int) and eta_s >= 0:
        agent_params["wait"] = wait
        agent_params["eta_s"] = eta_s
    message = message_for(blocking, agents, gate_objects, action_points)
    if message:
        agent_params["message"] = message

    gate_texts = [_item_text(gate) for gate in gate_objects]
    tokens = {
        "closing_completion": completion,
        "closing_parse": parse_status,
        "session_title": (title or "")[:120],
    }
    if dependencies_authoritative:
        tokens.update(
            {
                "closing_blocking": str(blocking),
                "closing_idle": "1" if state == "idle" else "0",
                "closing_agent_names": "; ".join(agent_names)[:200],
                "closing_gates": "; ".join(gate_texts)[:200],
                "closing_wait": (external_wait or "")[:200],
                "closing_workers_unknown": "1" if workers_unknown else "0",
            }
        )
        if reported_agents is not None:
            tokens["closing_agents"] = str(reported_agents)
        if contract and isinstance(contract_met, bool):
            tokens["closing_contract"] = contract[:200]
            tokens["closing_contract_met"] = "1" if contract_met else "0"
    meta_params = {
        "pane_id": pane_id,
        "source": source,
        "agent": agent,
        "applies_to_source": source,
        "tokens": tokens,
        "state_labels": {
            "blocked": blocked_state_label(
                blocking,
                [
                    item
                    for item in all_items
                    if str(item.get("label") or "").casefold()
                    in _HUMAN_INPUT_LABELS
                ],
            ),
            "working": "working",
        },
        "seq": seq,
    }
    if session_id:
        agent_params["agent_session_id"] = session_id
        meta_params["agent_session_id"] = session_id

    session_delivered = _rpc(
        sock_path, source, "pane.report_agent_session", session_params
    )
    report_delivered = _rpc(sock_path, source, "pane.report_agent", agent_params)
    metadata_delivered = _rpc(sock_path, source, "pane.report_metadata", meta_params)
    outcome["socket"] = bool(
        session_delivered and report_delivered and metadata_delivered
    )
    return outcome
