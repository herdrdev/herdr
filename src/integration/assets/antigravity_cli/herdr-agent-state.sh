#!/bin/sh
# installed by herdr
# managed by herdr; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# HERDR_INTEGRATION_ID=antigravity_cli
# HERDR_INTEGRATION_VERSION=2

# Session-only: this hook reports the Antigravity conversation so Herdr can
# resume the pane. Lifecycle state comes from Herdr's screen detection.

set -eu

# Antigravity CLI expects a JSON object on stdout and this hook never injects
# anything, so every exit path emits an empty object.
emit_and_exit() {
  printf '{}\n'
  exit 0
}

[ "${1:-}" = "session" ] || emit_and_exit
[ "${HERDR_ENV:-}" = "1" ] || emit_and_exit
[ -n "${HERDR_SOCKET_PATH:-}" ] || emit_and_exit
[ -n "${HERDR_PANE_ID:-}" ] || emit_and_exit
command -v python3 >/dev/null 2>&1 || emit_and_exit

python3 -c '
import json
import os
import socket
import sys
import time

try:
    payload = json.load(sys.stdin)
except Exception:
    raise SystemExit(0)

if not isinstance(payload, dict):
    raise SystemExit(0)

def text(name):
    value = payload.get(name)
    return value if isinstance(value, str) and value else None

session_id = text("conversationId")
if session_id is None:
    raise SystemExit(0)

seq = time.time_ns()
params = {
    "pane_id": os.environ["HERDR_PANE_ID"],
    "source": "herdr:antigravity_cli",
    "agent": "agy",
    "seq": seq,
    "agent_session_id": session_id,
}

transcript_path = text("transcriptPath")
if transcript_path is not None:
    params["agent_session_path"] = transcript_path

request = json.dumps({
    "id": f"herdr:antigravity_cli:{seq}",
    "method": "pane.report_agent_session",
    "params": params,
})
try:
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
        client.settimeout(0.5)
        client.connect(os.environ["HERDR_SOCKET_PATH"])
        client.sendall((request + "\n").encode())
        client.recv(4096)
except Exception:
    pass
' 2>/dev/null || true

emit_and_exit
