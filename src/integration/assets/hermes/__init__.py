"""Hermes plugin installed by Herdr to report session identity and agent state."""

# HERDR_INTEGRATION_ID=hermes
# HERDR_INTEGRATION_VERSION=6

from __future__ import annotations

import os
import subprocess
import threading
import time

_SOURCE = "herdr:hermes"
_AGENT = "hermes"
_INTERACTIVE_PLATFORMS = {"cli", "tui", "desktop", "acp"}

# herdr-core (hc) identifies agents purely from OSC 7501 status reports written
# to the terminal's tty: ESC ]7501;state=<state>:app=hermes ESC \ with states
# idle, working, done, blocked, or error. herdr-core exports HC_TERMINAL_ID (not
# HERDR_*) to the processes it starts; when it is set, this plugin reports state
# over OSC 7501 instead of the herdr CLI. `done` is the turn-completion signal:
# it releases waiting `hc run` callers, bumps the terminal's turn counter, and
# persists until the next report, so completed turns stay at `done` and only
# session boundaries or interrupted turns return to `idle`.
_APP = "hermes"
_TERMINAL_ID_ENV_VAR = "HC_TERMINAL_ID"
_MAX_ANCESTOR_HOPS = 12

_emit_lock = threading.Lock()
_last: str | None = None


def _osc_enabled() -> bool:
    return bool(os.environ.get(_TERMINAL_ID_ENV_VAR, "").strip())


def _interactive_platform(kwargs) -> bool:
    """True when the hook belongs to a session attached to a user's terminal.

    Hermes fires some hooks for subagent and other non-interactive turns; those
    must not overwrite the state of the session the user is watching. Hooks
    without a platform (or with an unknown empty one) pass: the environment gate
    already scopes reporting to processes a terminal launched.
    """
    platform = kwargs.get("platform")
    return not platform or platform in _INTERACTIVE_PLATFORMS


def _tty_candidates() -> list[str]:
    """Terminal devices to try, in order: /dev/tty, then each ancestor's stdout pty.

    Hermes runs turns and plugin hooks in a gateway child that has no
    controlling terminal, so /dev/tty fails there with ENXIO. The ancestor that
    is attached to the Herdr terminal still owns the pty on fd 1. The walk needs
    /proc, so on platforms without it /dev/tty remains the only candidate.
    """
    paths = ["/dev/tty"]
    pid = os.getpid()
    for _ in range(_MAX_ANCESTOR_HOPS):
        try:
            with open(f"/proc/{pid}/stat", encoding="utf-8") as f:
                ppid = int(f.read().rsplit(")", 1)[1].split()[1])
        except (OSError, ValueError, IndexError):
            break
        if ppid <= 1:
            break
        try:
            target = os.readlink(f"/proc/{ppid}/fd/1")
        except OSError:
            target = ""
        if target.startswith("/dev/pts/") and target not in paths:
            paths.append(target)
        pid = ppid
    return paths


def _emit(state: str) -> None:
    """Report `state` to herdr-core. Never raises; drops the report when no tty accepts it."""
    global _last
    try:
        if not _osc_enabled():
            return
        with _emit_lock:
            if state == _last:
                return
            seq = f"\x1b]7501;state={state}:app={_APP}\x1b\\".encode()
            for path in _tty_candidates():
                try:
                    # O_NONBLOCK keeps a stalled terminal reader from parking a
                    # Hermes hook worker: a status report is best-effort, so an
                    # EAGAIN drops it and the next hook retries.
                    fd = os.open(path, os.O_WRONLY | os.O_NOCTTY | os.O_NONBLOCK)
                    try:
                        view = seq
                        while view:
                            written = os.write(fd, view)
                            if written <= 0:
                                raise OSError("tty write made no progress")
                            view = view[written:]
                    finally:
                        os.close(fd)
                except OSError:
                    continue
                _last = state
                return
    except Exception:
        pass


def _pane_id() -> str | None:
    if os.environ.get("HERDR_ENV") != "1":
        return None
    return os.environ.get("HERDR_PANE_ID", "").strip() or None


def _send_session(session_id: str, start_source: str) -> None:
    pane_id = _pane_id()
    if pane_id is None:
        return
    command = [
        os.environ.get("HERDR_BIN_PATH") or "herdr",
        "pane",
        "report-agent-session",
        pane_id,
        "--source",
        _SOURCE,
        "--agent",
        _AGENT,
        "--seq",
        str(time.time_ns()),
        "--agent-session-id",
        session_id,
        "--session-start-source",
        start_source,
    ]
    try:
        kwargs = {"timeout": 1, "stdout": subprocess.DEVNULL, "stderr": subprocess.DEVNULL}
        if os.name == "nt":
            kwargs["creationflags"] = subprocess.CREATE_NO_WINDOW
        subprocess.run(command, check=False, **kwargs)
    except Exception:
        pass


def _report_session(start_source: str, **kwargs) -> None:
    if kwargs.get("platform") not in _INTERACTIVE_PLATFORMS:
        return
    session_id = kwargs.get("session_id")
    if not isinstance(session_id, str) or not session_id:
        return
    _send_session(session_id, start_source)


def _session_started(**kwargs) -> None:
    _report_session("startup", **kwargs)
    if _interactive_platform(kwargs):
        _emit("idle")


def _session_reset(**kwargs) -> None:
    _report_session("new", **kwargs)
    if _interactive_platform(kwargs):
        _emit("idle")


def _session_observed(**kwargs) -> None:
    if kwargs.get("platform") == "cli":
        _report_session("resume", **kwargs)
    if _interactive_platform(kwargs):
        _emit("working")


def _turn_completed(**kwargs) -> None:
    if _interactive_platform(kwargs):
        _emit("done")


def _human_input_requested(**kwargs) -> None:
    if _interactive_platform(kwargs):
        _emit("blocked")


def _human_input_resolved(**kwargs) -> None:
    if _interactive_platform(kwargs):
        _emit("working")


def _approval_requested(**kwargs) -> None:
    _emit("blocked")


def _approval_responded(**kwargs) -> None:
    _emit("working")


def _session_ended(**kwargs) -> None:
    # A completed turn already reported `done` from post_llm_call; a turn that
    # ended interrupted or failed never completed, so rest at `idle` instead.
    if kwargs.get("completed"):
        return
    if _interactive_platform(kwargs):
        _emit("idle")


def _agent_loop_stopped(**kwargs) -> None:
    if _interactive_platform(kwargs):
        _emit("idle")


def register(ctx):
    ctx.register_hook("on_session_start", _session_started)
    ctx.register_hook("on_session_reset", _session_reset)
    ctx.register_hook("pre_llm_call", _session_observed)
    ctx.register_hook("post_llm_call", _turn_completed)
    ctx.register_hook("on_human_input_request", _human_input_requested)
    ctx.register_hook("on_human_input_resolved", _human_input_resolved)
    ctx.register_hook("pre_approval_request", _approval_requested)
    ctx.register_hook("post_approval_response", _approval_responded)
    ctx.register_hook("on_session_end", _session_ended)
    ctx.register_hook("agent_loop_stopped", _agent_loop_stopped)
    # Show the terminal as a resting agent as soon as the plugin loads, before
    # the first turn fires on_session_start. No-op outside herdr-core.
    _emit("idle")
