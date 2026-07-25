"""Hermes plugin installed by Herdr to report resumable session identity."""

# HERDR_INTEGRATION_ID=hermes
# HERDR_INTEGRATION_VERSION=4

from __future__ import annotations

import json
import os
import random
import socket
import time

_SOURCE = "herdr:hermes"
_AGENT = "hermes"
_INTERACTIVE_PLATFORMS = {"cli", "tui", "desktop", "acp"}


def _base_params() -> tuple[str, str] | None:
    if os.environ.get("HERDR_ENV") != "1":
        return None
    pane_id = os.environ.get("HERDR_PANE_ID", "").strip()
    socket_path = os.environ.get("HERDR_SOCKET_PATH", "").strip()
    if not pane_id or not socket_path:
        return None
    return pane_id, socket_path


def _send(method: str, params: dict) -> None:
    base = _base_params()
    if base is None:
        return
    pane_id, socket_path = base
    params = {
        "pane_id": pane_id,
        "source": _SOURCE,
        "agent": _AGENT,
        "seq": time.time_ns(),
        **params,
    }
    request = {
        "id": f"{_SOURCE}:{int(time.time() * 1000)}:{random.randrange(1_000_000):06d}",
        "method": method,
        "params": params,
    }
    try:
        client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        client.settimeout(0.5)
        client.connect(socket_path)
        client.sendall((json.dumps(request) + "\n").encode("utf-8"))
        try:
            client.recv(4096)
        except Exception:
            pass
        client.close()
    except Exception:
        pass


def _report_session(start_source: str, **kwargs) -> None:
    if kwargs.get("platform") not in _INTERACTIVE_PLATFORMS:
        return
    session_id = kwargs.get("session_id")
    if not isinstance(session_id, str) or not session_id:
        return
    _send(
        "pane.report_agent_session",
        {
            "agent_session_id": session_id,
            "session_start_source": start_source,
        },
    )


def _session_started(**kwargs) -> None:
    _report_session("startup", **kwargs)


def _session_reset(**kwargs) -> None:
    _report_session("new", **kwargs)


def _session_observed(**kwargs) -> None:
    if kwargs.get("platform") == "cli":
        _report_session("resume", **kwargs)


def register(ctx):
    ctx.register_hook("on_session_start", _session_started)
    ctx.register_hook("on_session_reset", _session_reset)
    ctx.register_hook("pre_llm_call", _session_observed)
