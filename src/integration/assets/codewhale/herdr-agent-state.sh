#!/bin/sh
# installed by herdr
# managed by herdr; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# HERDR_INTEGRATION_ID=codewhale
# HERDR_INTEGRATION_VERSION=1

set -eu

action="${1:-}"
case "$action" in
  session_start|message_submit|tool_call_before|tool_call_after|turn_end|on_error|session_end|session|working|blocked|idle|release) ;;
  *) exit 0 ;;
esac

[ "${HERDR_ENV:-}" = "1" ] || exit 0
[ -n "${HERDR_PANE_ID:-}" ] || exit 0
[ -n "${HERDR_SOCKET_PATH:-}" ] || [ -n "${HERDR_BIN_PATH:-}" ] || exit 0
command -v python3 >/dev/null 2>&1 || exit 0

python3 - "$action" <<'PY'
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import time

action = sys.argv[1]
pane_id = os.environ.get("HERDR_PANE_ID")
socket_path = os.environ.get("HERDR_SOCKET_PATH")
herdr_bin = os.environ.get("HERDR_BIN_PATH") or "herdr"

if not pane_id:
    sys.exit(0)

payload = {}
if not sys.stdin.isatty():
    try:
        raw = sys.stdin.read()
        if raw and raw.strip():
            payload = json.loads(raw)
    except Exception:
        payload = {}

# Session ID discovery
session_id = (
    payload.get("session_id")
    or os.environ.get("CODEWHALE_SESSION_ID")
    or os.environ.get("DEEPSEEK_SESSION_ID")
)
if isinstance(session_id, str):
    session_id = session_id.strip() or None
else:
    session_id = None

# If not in env/payload, try checking ~/.codewhale/sessions for matching workspace
if not session_id:
    try:
        sessions_dir = Path.home() / ".codewhale" / "sessions"
        cwd = os.environ.get("DEEPSEEK_WORKSPACE") or os.getcwd()
        if sessions_dir.is_dir():
            candidates = sorted(sessions_dir.glob("*.json"), key=lambda p: p.stat().st_mtime, reverse=True)
            for cand in candidates[:5]:
                try:
                    with cand.open("r", encoding="utf-8") as f:
                        meta = json.load(f).get("metadata", {})
                        if meta.get("workspace") == cwd and meta.get("id"):
                            session_id = meta.get("id")
                            break
                except Exception:
                    continue
    except Exception:
        pass

source = "herdr:codewhale"
agent = "codewhale"
seq = time.time_ns()

requests = []

if action in ("session_end", "release"):
    requests.append({
        "id": f"{source}:{seq}:release",
        "method": "pane.release_agent",
        "params": {
            "pane_id": pane_id,
            "source": source,
            "agent": agent,
            "seq": seq,
        },
    })
elif action in ("session_start", "session"):
    if session_id:
        requests.append({
            "id": f"{source}:{seq}:session",
            "method": "pane.report_agent_session",
            "params": {
                "pane_id": pane_id,
                "source": source,
                "agent": agent,
                "seq": seq,
                "agent_session_id": session_id,
                "session_start_source": "startup",
            },
        })
    state_params = {
        "pane_id": pane_id,
        "source": source,
        "agent": agent,
        "seq": seq + 1,
        "state": "idle",
    }
    if session_id:
        state_params["agent_session_id"] = session_id
    requests.append({
        "id": f"{source}:{seq}:state",
        "method": "pane.report_agent",
        "params": state_params,
    })
else:
    tool_name = os.environ.get("DEEPSEEK_TOOL_NAME") or os.environ.get("CODEWHALE_TOOL_NAME")
    if action in ("message_submit", "session_busy", "working", "tool_call_after"):
        state = "working"
        message = None
    elif action == "tool_call_before":
        if tool_name in ("request_user_input", "ask_user"):
            state = "blocked"
            message = "Waiting for user input"
        else:
            state = "working"
            message = None
    elif action in ("waiting_for_user", "blocked"):
        state = "blocked"
        reason = payload.get("reason")
        if reason == "approval":
            message = "Waiting for tool approval"
        elif reason == "user_input":
            message = "Waiting for user input"
        elif reason == "goal_continuation":
            message = "Goal continuation parked"
        elif isinstance(reason, str) and reason:
            message = f"Waiting for {reason}"
        else:
            message = "Waiting for user decision"
    else:  # turn_end, on_error, idle
        state = "idle"
        message = None

    params = {
        "pane_id": pane_id,
        "source": source,
        "agent": agent,
        "seq": seq,
        "state": state,
    }
    if session_id:
        params["agent_session_id"] = session_id
    if message:
        params["message"] = message

    requests.append({
        "id": f"{source}:{seq}:state",
        "method": "pane.report_agent",
        "params": params,
    })

# First attempt: Direct socket connection (fastest)
socket_success = False
if socket_path and os.path.exists(socket_path):
    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
            client.settimeout(0.5)
            client.connect(socket_path)
            for req in requests:
                client.sendall((json.dumps(req) + "\n").encode())
                client.recv(4096)
            socket_success = True
    except Exception:
        socket_success = False

# Fallback: Herdr CLI execution
if not socket_success:
    for req in requests:
        m = req["method"]
        p = req["params"]
        try:
            if m == "pane.release_agent":
                subprocess.run([
                    herdr_bin, "pane", "release-agent",
                    "--source", p["source"],
                    "--agent", p["agent"],
                    "--seq", str(p["seq"]),
                    p["pane_id"],
                ], timeout=1.0, check=False)
            elif m == "pane.report_agent_session":
                cmd = [
                    herdr_bin, "pane", "report-agent-session",
                    "--source", p["source"],
                    "--agent", p["agent"],
                    "--seq", str(p["seq"]),
                    "--agent-session-id", p["agent_session_id"],
                ]
                if "session_start_source" in p:
                    cmd.extend(["--session-start-source", p["session_start_source"]])
                cmd.append(p["pane_id"])
                subprocess.run(cmd, timeout=1.0, check=False)
            elif m == "pane.report_agent":
                cmd = [
                    herdr_bin, "pane", "report-agent",
                    "--source", p["source"],
                    "--agent", p["agent"],
                    "--seq", str(p["seq"]),
                    "--state", p["state"],
                ]
                if "agent_session_id" in p:
                    cmd.extend(["--agent-session-id", p["agent_session_id"]])
                if "message" in p:
                    cmd.extend(["--message", p["message"]])
                cmd.append(p["pane_id"])
                subprocess.run(cmd, timeout=1.0, check=False)
        except Exception:
            pass
PY
